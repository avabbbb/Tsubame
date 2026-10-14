//! Single-sentence regenerate loop (PR plan #11): new take → spatialize →
//! downstream replan, A/B take switching, pushed flags and the context preview.
//!
//! Contract: `docs/20-segment-regenerate.md`.
//!
//! - `regenerate_segment` runs as job kind `regenerate` (stages `tts`,
//!   `spatialize`, `replan`, `done`).
//! - The take commit, take pruning and the downstream plan diff (pushed flags)
//!   happen in one DB transaction, so a cancel or a spatial failure afterwards
//!   never leaves a new clip without its pushed flags.
//! - Placement stays derived (`placement::replan` with the confirmed default
//!   rules); only `segments.dub_pushed_ms` is persisted, as a UI annotation that
//!   does not bump `revision`.

use crate::db::{MediaDb, Segment};
use crate::jobs::{JobError, JobHandle, JobRunner, NewJob, NoopSink, ProgressSink};
use crate::mix::{self, ExportKind, MixOptions};
use crate::placement::{self, DubSentenceRow, Placement, PlacementParams};
use crate::spatial::{self, RenderWindow, SpatialMode, Strengths};
use crate::takes::{self, PruneResult, TakeInfo};
use crate::tts::{self, AssetStore, PreparedSynthesis, TtsAdapter, TtsAsset, TtsError};
use crate::voice::VoiceProfile;
use crate::worker::WorkerSpec;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use std::sync::Mutex;

/// Job kind of `regenerate_segment` runs.
pub const REGENERATE_JOB: &str = "regenerate";
pub const STAGE_TTS: &str = "tts";
pub const STAGE_SPATIALIZE: &str = "spatialize";
pub const STAGE_REPLAN: &str = "replan";
pub const STAGE_DONE: &str = "done";
/// `progress_total` of a regenerate job (one unit per stage).
pub const STAGE_COUNT: i64 = 3;

pub const DEFAULT_PRE_ROLL_MS: i64 = 1_000;
pub const MAX_PRE_ROLL_MS: i64 = 30_000;
/// Silence/original kept after the placed dub ends.
pub const PREVIEW_TAIL_MS: i64 = 250;
/// Newest preview files kept in `<assets>/preview`.
pub const MAX_PREVIEW_FILES: usize = 8;

pub const SPATIAL_DONE: &str = "done";
pub const SPATIAL_SKIPPED: &str = "skipped";
pub const SPATIAL_FAILED: &str = "failed";

fn dberr(error: impl std::fmt::Display) -> TtsError {
    TtsError::new("db", error.to_string(), true)
}

fn input_err(message: impl Into<String>) -> TtsError {
    TtsError::new("invalid_input", message, false)
}

fn not_found(message: &str) -> TtsError {
    TtsError::new("not_found", message, false)
}

// ----- plan change -----

/// One downstream Segment whose placed start changed.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PlanShift {
    pub segment_id: i64,
    pub old_start_ms: i64,
    pub new_start_ms: i64,
    /// `new_start_ms - old_start_ms` (> 0 = pushed later).
    pub delta_ms: i64,
    pub drift_ms: i64,
    pub drift_exceeded: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PlanChange {
    /// Timeline index of the edited Segment.
    pub index: usize,
    /// The edited Segment's new placement.
    pub edited: Option<Placement>,
    /// `[start, end)` indices `placement::replan` recomputed.
    pub recomputed_start: usize,
    pub recomputed_end: usize,
    /// Downstream Segments whose start changed.
    pub moved: Vec<PlanShift>,
    /// The subset of `moved` that now starts later (UI: yellow until acknowledged).
    pub pushed: Vec<PlanShift>,
}

impl PlanChange {
    fn unchanged(index: usize) -> Self {
        Self {
            index,
            edited: None,
            recomputed_start: index,
            recomputed_end: index,
            moved: Vec::new(),
            pushed: Vec::new(),
        }
    }
}

/// Placement inputs and plan of a media item before a clip change.
#[derive(Debug, Clone)]
pub struct PlanSnapshot {
    pub rows: Vec<DubSentenceRow>,
    pub plan: Vec<Placement>,
    pub index: usize,
}

pub fn snapshot(
    conn: &Connection,
    media_id: i64,
    segment_id: i64,
) -> Result<PlanSnapshot, TtsError> {
    let rows = placement::load_dub_sentences(conn, media_id).map_err(dberr)?;
    let sentences = rows.iter().map(|r| r.sentence.clone()).collect::<Vec<_>>();
    let plan = placement::place(&sentences, &PlacementParams::default()).map_err(input_err)?;
    let index = rows
        .iter()
        .position(|row| row.sentence.segment_id == segment_id)
        .ok_or_else(|| not_found("segment not found in its media timeline"))?;
    Ok(PlanSnapshot { rows, plan, index })
}

/// Replans after the edited Segment's clip changed to `new_duration` and
/// persists the pushed flags (`dub_pushed_ms += delta`, cleared when ≤ 0).
/// Only Segments after the edited one are recomputed.
pub fn apply_plan_change(
    conn: &Connection,
    snap: &PlanSnapshot,
    new_duration: Option<i64>,
) -> Result<PlanChange, TtsError> {
    let sentences = snap
        .rows
        .iter()
        .map(|r| r.sentence.clone())
        .collect::<Vec<_>>();
    let replan = placement::replan(
        &snap.plan,
        &sentences,
        &PlacementParams::default(),
        snap.index,
        new_duration,
    )
    .map_err(input_err)?;
    let mut moved = Vec::new();
    for i in replan.recomputed.clone() {
        if i <= snap.index {
            continue;
        }
        let (old, new) = (&snap.plan[i], &replan.plan[i]);
        if old.is_placed() && new.is_placed() && old.dub_start_ms != new.dub_start_ms {
            moved.push(PlanShift {
                segment_id: new.segment_id,
                old_start_ms: old.dub_start_ms,
                new_start_ms: new.dub_start_ms,
                delta_ms: new.dub_start_ms - old.dub_start_ms,
                drift_ms: new.drift_ms,
                drift_exceeded: new.drift_exceeded(),
            });
        }
    }
    for shift in &moved {
        conn.execute(
            "UPDATE segments
                SET dub_pushed_ms = CASE WHEN dub_pushed_ms + ?1 > 0 THEN dub_pushed_ms + ?1 ELSE 0 END
              WHERE id = ?2",
            params![shift.delta_ms, shift.segment_id],
        )
        .map_err(dberr)?;
    }
    let pushed = moved.iter().filter(|s| s.delta_ms > 0).cloned().collect();
    Ok(PlanChange {
        index: snap.index,
        edited: Some(replan.plan[snap.index].clone()),
        recomputed_start: replan.recomputed.start,
        recomputed_end: replan.recomputed.end,
        moved,
        pushed,
    })
}

/// Clears the pushed flag of the given Segments; returns the ones that exist.
pub fn clear_pushed(db: &MediaDb, segment_ids: &[i64]) -> Result<Vec<Segment>, TtsError> {
    let mut out = Vec::new();
    for id in segment_ids {
        db.connection()
            .execute("UPDATE segments SET dub_pushed_ms=0 WHERE id=?1", [id])
            .map_err(dberr)?;
        if let Some(segment) = db.get_segment(*id).map_err(dberr)? {
            out.push(segment);
        }
    }
    Ok(out)
}

// ----- commits -----

/// Result of the regenerate commit transaction.
#[derive(Debug, Clone)]
pub struct Committed {
    pub segment: Segment,
    pub previous_key: Option<String>,
    pub change: PlanChange,
    pub prune: PruneResult,
    pub snapshot: PlanSnapshot,
}

/// One transaction: point the Segment at the new take (revision-checked),
/// record the take, prune old takes, replan downstream and persist pushed flags.
pub fn commit_regenerated(
    db: &MediaDb,
    prepared: &PreparedSynthesis,
    take: u32,
    asset: &TtsAsset,
    expected_revision: i64,
) -> Result<Committed, TtsError> {
    let conn = db.connection();
    let tx = conn.unchecked_transaction().map_err(dberr)?;
    let snap = snapshot(conn, prepared.segment.media_id, prepared.segment.id)?;
    let previous_key = snap.rows[snap.index].tts_asset_key.clone();
    tts::commit_segment(db, prepared, expected_revision)?;
    takes::record_take(conn, prepared, take).map_err(dberr)?;
    let prune = takes::prune(conn, prepared.segment.id).map_err(dberr)?;
    let change = apply_plan_change(conn, &snap, Some(asset.duration_ms))?;
    tx.commit().map_err(dberr)?;
    let segment = db
        .get_segment(prepared.segment.id)
        .map_err(dberr)?
        .ok_or_else(|| not_found("segment not found"))?;
    Ok(Committed {
        segment,
        previous_key,
        change,
        prune,
        snapshot: snap,
    })
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ActiveTakeOutcome {
    pub segment: Segment,
    pub take: Option<TakeInfo>,
    pub plan_change: PlanChange,
}

/// A/B / revert: makes one of the Segment's takes active (revision-checked)
/// and replans downstream. Selecting the active take is a no-op.
pub fn activate_take(
    db: &MediaDb,
    store: &AssetStore,
    segment_id: i64,
    take_key: &str,
    expected_revision: i64,
) -> Result<ActiveTakeOutcome, TtsError> {
    let conn = db.connection();
    let segment = db
        .get_segment(segment_id)
        .map_err(dberr)?
        .ok_or_else(|| not_found("segment not found"))?;
    if segment.revision != expected_revision {
        return Err(TtsError::new(
            "revision_conflict",
            format!(
                "expected r{expected_revision}, sentence is at r{}",
                segment.revision
            ),
            false,
        ));
    }
    let take = takes::find_take(conn, segment_id, take_key)
        .map_err(dberr)?
        .ok_or_else(|| not_found("take not found for this sentence"))?;
    let asset = tts::verified_asset(conn, store, take_key)?.ok_or_else(|| {
        TtsError::new(
            "asset_missing",
            "the take's audio is missing; regenerate the sentence",
            true,
        )
    })?;
    let info = |conn: &Connection| takes::take_info(conn, store, segment_id, take_key);
    if segment.tts_asset_key.as_deref() == Some(take_key) {
        let snap = snapshot(conn, segment.media_id, segment_id)?;
        return Ok(ActiveTakeOutcome {
            take: info(conn).map_err(dberr)?,
            plan_change: PlanChange::unchanged(snap.index),
            segment,
        });
    }
    // Unknown take text (legacy backfill) is treated as stale.
    let dirty_tts =
        take.text.is_empty() || take.text != tts::normalize_text(&segment.translated_text);
    let tx = conn.unchecked_transaction().map_err(dberr)?;
    let snap = snapshot(conn, segment.media_id, segment_id)?;
    let changed = conn
        .execute(
            "UPDATE segments
                SET tts_asset_key=?1, tts_provenance=?2, dirty_tts=?3, dirty_mix=1,
                    revision=revision+1
              WHERE id=?4 AND revision=?5",
            params![
                take_key,
                format!("tts:{}:{}", asset.provider_id, asset.model_id),
                dirty_tts,
                segment_id,
                expected_revision
            ],
        )
        .map_err(dberr)?;
    if changed == 0 {
        return Err(tts::conflict_or_missing(db, segment_id));
    }
    let change = apply_plan_change(conn, &snap, Some(asset.duration_ms))?;
    tx.commit().map_err(dberr)?;
    Ok(ActiveTakeOutcome {
        segment: db
            .get_segment(segment_id)
            .map_err(dberr)?
            .ok_or_else(|| not_found("segment not found"))?,
        take: info(conn).map_err(dberr)?,
        plan_change: change,
    })
}

// ----- regenerate job -----

/// `input_json` of a `regenerate` job.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegenerateInput {
    pub segment_id: i64,
    pub expected_revision: i64,
    pub voice_profile_id: String,
}

/// Whether the spatialize stage can run.
#[derive(Debug, Clone)]
pub enum SpatialAvailability {
    Available(WorkerSpec),
    /// Reason slug + message, reported as the stage's `reason`.
    Unavailable(String),
}

fn program_on_path(program: &Path) -> bool {
    if program.components().count() > 1 {
        return program.is_file();
    }
    std::env::var_os("PATH")
        .is_some_and(|paths| std::env::split_paths(&paths).any(|dir| dir.join(program).is_file()))
}

/// The spatial worker is available when its Python program resolves and its
/// script exists (`TSUBAME_PYTHON`, `TSUBAME_SPATIAL_WORKER`; docs/19).
pub fn spatial_availability() -> SpatialAvailability {
    let program = spatial::python_program();
    let script = spatial::worker_script();
    if !program_on_path(&program) {
        return SpatialAvailability::Unavailable(format!(
            "spatial_worker_unavailable: python program {} not found",
            program.display()
        ));
    }
    if !script.is_file() {
        return SpatialAvailability::Unavailable(format!(
            "spatial_worker_unavailable: worker script {} not found",
            script.display()
        ));
    }
    SpatialAvailability::Available(spatial::worker_spec())
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SpatialRenderReport {
    pub segment_id: i64,
    pub mode: SpatialMode,
    pub render_key: String,
    pub cache_hit: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SpatialError {
    pub segment_id: Option<i64>,
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SpatialStageReport {
    /// `done` | `skipped` | `failed`.
    pub status: String,
    pub reason: Option<String>,
    pub renders: Vec<SpatialRenderReport>,
    pub errors: Vec<SpatialError>,
}

impl SpatialStageReport {
    fn skipped(reason: String) -> Self {
        Self {
            status: SPATIAL_SKIPPED.into(),
            reason: Some(reason),
            renders: Vec::new(),
            errors: Vec::new(),
        }
    }
    fn fail(&mut self, segment_id: Option<i64>, error: &JobError) {
        self.status = SPATIAL_FAILED.into();
        if self.reason.is_none() {
            self.reason = Some(format!("{}: {}", error.code, error.message));
        }
        self.errors.push(SpatialError {
            segment_id,
            code: error.code.clone(),
            message: error.message.clone(),
        });
    }
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RegenerateOutcome {
    pub segment: Segment,
    pub take: Option<TakeInfo>,
    pub take_index: u32,
    pub cache_hit: bool,
    pub previous_tts_asset_key: Option<String>,
    pub plan_change: PlanChange,
    pub spatial: SpatialStageReport,
    pub pruned_takes: Vec<i64>,
    pub deleted_assets: Vec<String>,
}

/// Forwards adapter progress as the job's `tts` stage.
struct TtsStageSink<'a>(&'a JobHandle);

impl ProgressSink for TtsStageSink<'_> {
    fn progress(&self, _stage: &str, _done: u64, _total: u64) {
        self.0.progress(0, STAGE_COUNT, STAGE_TTS);
    }
    fn is_cancelled(&self) -> bool {
        self.0.is_cancelled()
    }
}

fn is_cancel(job: &JobHandle, error: &JobError) -> bool {
    error.code == "cancelled" || job.is_cancelled()
}

fn check_job_cancelled(job: &JobHandle) -> Result<(), JobError> {
    if job.is_cancelled() {
        Err(JobError::cancelled())
    } else {
        Ok(())
    }
}

fn verified_render(conn: &Connection, store: &AssetStore, key: &str) -> Result<bool, JobError> {
    spatial::verified_asset(conn, store, key, spatial::KIND_RENDER)
        .map(|found| found.is_some())
        .map_err(|e| JobError::new("db", true, e))
}

fn concurrent_key(
    tts_key: &str,
    track_key: &str,
    row: &DubSentenceRow,
    dub_start_ms: i64,
) -> Result<String, JobError> {
    spatial::render_key(
        tts_key,
        track_key,
        SpatialMode::Concurrent,
        &RenderWindow {
            src_start_ms: row.sentence.src_start_ms.max(0),
            src_end_ms: row.sentence.src_end_ms,
            dub_start_ms: dub_start_ms.max(0),
        },
        &Strengths::default(),
        spatial::RENDERER_VERSION,
    )
    .map_err(|e| JobError::new("internal", false, e))
}

/// Moved downstream Segments whose `concurrent` render must be redone: a
/// verified render exists under the old key (old `dub_start_ms`) and none under
/// the new one. `source_trajectory` keys do not contain the start, so they are
/// never redone for a move.
pub fn concurrent_rerender_targets(
    conn: &Connection,
    store: &AssetStore,
    track_key: &str,
    committed: &Committed,
) -> Result<Vec<i64>, JobError> {
    let mut targets = Vec::new();
    for shift in &committed.change.moved {
        let Some(row) = committed
            .snapshot
            .rows
            .iter()
            .find(|row| row.sentence.segment_id == shift.segment_id)
        else {
            continue;
        };
        let Some(tts_key) = row.tts_asset_key.as_deref() else {
            continue;
        };
        let old = concurrent_key(tts_key, track_key, row, shift.old_start_ms)?;
        let new = concurrent_key(tts_key, track_key, row, shift.new_start_ms)?;
        if verified_render(conn, store, &old)? && !verified_render(conn, store, &new)? {
            targets.push(shift.segment_id);
        }
    }
    Ok(targets)
}

/// Spatial work of a regenerate after the commit. Returns `Err` only on cancel.
fn spatial_work(
    db: &Mutex<MediaDb>,
    store: &AssetStore,
    spec: &WorkerSpec,
    job: &JobHandle,
    committed: &Committed,
) -> Result<SpatialStageReport, JobError> {
    let mut report = SpatialStageReport {
        status: SPATIAL_DONE.into(),
        reason: None,
        renders: Vec::new(),
        errors: Vec::new(),
    };
    let segment = &committed.segment;
    let render = |report: &mut SpatialStageReport,
                  segment_id: i64,
                  mode: SpatialMode|
     -> Result<(), JobError> {
        match spatial::spatialize(db, store, spec, job, segment_id, mode) {
            Ok(outcome) => report.renders.push(SpatialRenderReport {
                segment_id,
                mode,
                render_key: outcome.render_key,
                cache_hit: outcome.cache_hit,
            }),
            Err(error) if is_cancel(job, &error) => return Err(JobError::cancelled()),
            Err(error) => report.fail(Some(segment_id), &error),
        }
        Ok(())
    };

    // Stage `spatialize`: the new take.
    job.progress(1, STAGE_COUNT, STAGE_SPATIALIZE);
    let analysis = match spatial::ensure_analysis(db, store, spec, job, segment.media_id) {
        Ok(analysis) => analysis,
        Err(error) if is_cancel(job, &error) => return Err(JobError::cancelled()),
        Err(error) => {
            report.fail(None, &error);
            job.progress(2, STAGE_COUNT, STAGE_REPLAN);
            return Ok(report);
        }
    };
    job.progress(1, STAGE_COUNT, STAGE_SPATIALIZE);
    render(&mut report, segment.id, SpatialMode::SourceTrajectory)?;
    if let Some(previous) = committed.previous_key.as_deref() {
        let snap = &committed.snapshot;
        let old_key = concurrent_key(
            previous,
            &analysis.spatial_track_key,
            &snap.rows[snap.index],
            snap.plan[snap.index].dub_start_ms,
        )?;
        let had_concurrent = verified_render(tts::lock(db)?.connection(), store, &old_key)?;
        if had_concurrent {
            render(&mut report, segment.id, SpatialMode::Concurrent)?;
        }
    }

    // Stage `replan`: concurrent renders of moved downstream Segments.
    check_job_cancelled(job)?;
    job.progress(2, STAGE_COUNT, STAGE_REPLAN);
    let targets = concurrent_rerender_targets(
        tts::lock(db)?.connection(),
        store,
        &analysis.spatial_track_key,
        committed,
    )?;
    for segment_id in targets {
        check_job_cancelled(job)?;
        render(&mut report, segment_id, SpatialMode::Concurrent)?;
    }
    Ok(report)
}

/// The whole regenerate job body (testable without Tauri).
#[allow(clippy::too_many_arguments)]
pub fn run_regenerate<A: TtsAdapter>(
    db: &Mutex<MediaDb>,
    store: &AssetStore,
    adapter: &A,
    profile: &VoiceProfile,
    spatial_availability: &SpatialAvailability,
    job: &JobHandle,
    input: &RegenerateInput,
) -> Result<RegenerateOutcome, JobError> {
    // Stage `tts`.
    job.progress(0, STAGE_COUNT, STAGE_TTS);
    let (prepared, take, cached) = {
        let guard = tts::lock(db)?;
        let take = takes::next_take_index(guard.connection(), input.segment_id)
            .map_err(|e| JobError::new("db", true, e.to_string()))?;
        let prepared = tts::prepare(
            &guard,
            input.segment_id,
            input.expected_revision,
            profile,
            adapter.adapter_version(),
            take,
        )?;
        let cached = tts::verified_asset(guard.connection(), store, &prepared.key)?;
        (prepared, take, cached)
    };
    let sink = TtsStageSink(job);
    let (asset, cache_hit) = match cached {
        Some(asset) => (asset, true),
        None => {
            tts::check_cancelled(&sink)?;
            let asset = tauri::async_runtime::block_on(tts::synthesize_asset(
                db, store, adapter, &prepared, &sink,
            ))?;
            (asset, false)
        }
    };
    // A cancel during `tts` commits nothing.
    tts::check_cancelled(&sink)?;
    let committed = {
        let guard = tts::lock(db)?;
        commit_regenerated(&guard, &prepared, take, &asset, input.expected_revision)?
    };
    takes::delete_files(store, &committed.prune.deleted_paths);

    // Stages `spatialize` + `replan`.
    check_job_cancelled(job)?;
    let spatial = match spatial_availability {
        SpatialAvailability::Available(spec) => spatial_work(db, store, spec, job, &committed)?,
        SpatialAvailability::Unavailable(reason) => {
            job.progress(1, STAGE_COUNT, STAGE_SPATIALIZE);
            SpatialStageReport::skipped(reason.clone())
        }
    };
    check_job_cancelled(job)?;
    job.progress(2, STAGE_COUNT, STAGE_REPLAN);
    job.progress(STAGE_COUNT, STAGE_COUNT, STAGE_DONE);

    let (segment, take_info) = {
        let guard = tts::lock(db)?;
        let segment = guard
            .get_segment(input.segment_id)
            .map_err(|e| JobError::new("db", true, e.to_string()))?
            .unwrap_or_else(|| committed.segment.clone());
        let info = takes::take_info(guard.connection(), store, input.segment_id, &prepared.key)
            .map_err(|e| JobError::new("db", true, e.to_string()))?;
        (segment, info)
    };
    Ok(RegenerateOutcome {
        segment,
        take: take_info,
        take_index: take,
        cache_hit,
        previous_tts_asset_key: committed.previous_key.clone(),
        plan_change: committed.change.clone(),
        spatial,
        pruned_takes: committed.prune.pruned_takes.clone(),
        deleted_assets: committed.prune.deleted_assets.clone(),
    })
}

/// Voice profile of the Segment's active take (asset row first, then the
/// newest take row).
pub fn default_voice_profile(conn: &Connection, segment_id: i64) -> Result<String, TtsError> {
    let (key, _) = tts::segment_dub_fields(conn, segment_id)
        .map_err(dberr)?
        .ok_or_else(|| not_found("segment not found"))?;
    if let Some(key) = key {
        if let Some(asset) = tts::get_asset(conn, &key).map_err(dberr)? {
            if !asset.voice_profile_id.trim().is_empty() {
                return Ok(asset.voice_profile_id);
            }
        }
    }
    takes::takes_for_segment(conn, segment_id)
        .map_err(dberr)?
        .into_iter()
        .map(|take| take.voice_profile_id)
        .find(|id| !id.trim().is_empty())
        .ok_or_else(|| input_err("choose a voice profile for this sentence"))
}

/// Runner for `regenerate` jobs (registered at startup; `retry_job` re-runs it).
pub fn regenerate_runner(app: tauri::AppHandle) -> JobRunner {
    std::sync::Arc::new(move |job: &JobHandle| {
        use tauri::Manager;
        let input: RegenerateInput = serde_json::from_value(job.input()).map_err(|e| {
            JobError::new(
                "invalid_input",
                false,
                format!("invalid regenerate job input: {e}"),
            )
        })?;
        let state = app.state::<crate::AppState>();
        let store = tts::asset_store(&app).map_err(|e| JobError::new("io", false, e))?;
        let (profile, adapter) = tts::resolve_profile_adapter(&app, &input.voice_profile_id)?;
        let outcome = run_regenerate(
            &state.db,
            &store,
            &adapter,
            &profile,
            &spatial_availability(),
            job,
            &input,
        )?;
        serde_json::to_value(outcome).map_err(|e| JobError::new("internal", false, e.to_string()))
    })
}

// ----- preview -----

/// Preview window `[start, end)` in ms of the original: from the sentence start
/// minus `pre_roll_ms` (clamped at 0) to the placed dub's end plus the tail.
pub fn preview_window(src_start_ms: i64, dub_end_ms: i64, pre_roll_ms: i64) -> (i64, i64) {
    let start = (src_start_ms - pre_roll_ms.max(0)).max(0);
    let end = (dub_end_ms + PREVIEW_TAIL_MS).max(start);
    (start, end)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SegmentPreview {
    pub segment_id: i64,
    /// Absolute path of the preview WAV (frontend: `convertFileSrc`).
    pub path: String,
    pub window_start_ms: i64,
    pub window_end_ms: i64,
    pub dub_start_ms: i64,
    pub dub_end_ms: i64,
    /// Where the dub starts inside the preview file.
    pub dub_offset_ms: i64,
    pub solo: bool,
    /// The spatial render was used (else the raw TTS clip).
    pub spatial: bool,
    pub duration_ms: i64,
}

fn preview_err(error: impl std::fmt::Display) -> TtsError {
    TtsError::new("preview_failed", error.to_string(), true)
}

/// Keeps the newest `keep` `preview-*.wav` files in `dir` (plus `current`).
pub fn cleanup_previews(dir: &Path, keep: usize, current: &Path) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    let mut files = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("preview-") && n.ends_with(".wav"))
        })
        .filter_map(|path| {
            let modified = std::fs::metadata(&path).and_then(|m| m.modified()).ok()?;
            Some((modified, path))
        })
        .collect::<Vec<_>>();
    files.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    let mut kept = 0usize;
    for (_, path) in files {
        if path == current {
            continue;
        }
        kept += 1;
        if kept >= keep {
            let _ = std::fs::remove_file(&path);
        }
    }
}

/// Renders the context preview of one Segment (docs/20 § Preview) and clears
/// its pushed flag.
pub fn render_preview(
    db: &Mutex<MediaDb>,
    store: &AssetStore,
    segment_id: i64,
    pre_roll_ms: i64,
    solo: bool,
    mode: SpatialMode,
) -> Result<SegmentPreview, TtsError> {
    if !(0..=MAX_PRE_ROLL_MS).contains(&pre_roll_ms) {
        return Err(input_err(format!(
            "pre_roll_ms must be within 0..={MAX_PRE_ROLL_MS}"
        )));
    }
    let (original, mut clip, placed, has_renders) = {
        let guard = tts::lock(db)?;
        let segment = guard
            .get_segment(segment_id)
            .map_err(dberr)?
            .ok_or_else(|| not_found("segment not found"))?;
        let inputs = mix::export_inputs(
            guard.connection(),
            segment.media_id,
            &PlacementParams::default(),
        )
        .map_err(preview_err)?;
        let placed = inputs
            .plan
            .iter()
            .find(|p| p.segment_id == segment_id && p.is_placed())
            .cloned()
            .ok_or_else(|| TtsError::new("not_placed", "sentence has no placed dub", false))?;
        let mut clip = inputs
            .clips
            .into_iter()
            .find(|clip| clip.segment_id == segment_id)
            .ok_or_else(|| TtsError::new("not_placed", "sentence has no placed dub", false))?;
        if clip.path.is_relative() {
            clip.path = store.absolute(&clip.path.to_string_lossy())?;
        }
        if !clip.path.is_file() {
            return Err(TtsError::new(
                "asset_missing",
                "TTS asset file is missing; synthesize the sentence again",
                true,
            ));
        }
        let has_renders =
            spatial::has_renders(guard.connection(), segment.media_id).map_err(dberr)?;
        (inputs.original_path, clip, placed, has_renders)
    };
    let mut spatial_used = false;
    if has_renders {
        let (_, track_key) = spatial::current_track_key(&original)
            .map_err(|e| TtsError::new(&e.code, e.message, e.retryable))?;
        let guard = tts::lock(db)?;
        let mut clips = [clip];
        spatial_used =
            spatial::apply_spatial_renders(guard.connection(), store, &mut clips, &track_key, mode)
                .map_err(dberr)?
                > 0;
        let [swapped] = clips;
        clip = swapped;
    }

    let (window_start, window_end) =
        preview_window(clip.src_start_ms, placed.dub_end_ms, pre_roll_ms);
    let original_ms = mix::wav_duration_ms(&original).map_err(preview_err)?;
    let dir = store.root.join("preview");
    std::fs::create_dir_all(&dir).map_err(preview_err)?;
    let crop = dir.join(format!(".crop-{}.wav", uuid::Uuid::new_v4()));
    let output = dir.join(format!("preview-{segment_id}-{}.wav", uuid::Uuid::new_v4()));
    let result = (|| -> Result<(), String> {
        mix::crop_wav(&original, &crop, window_start, window_end.min(original_ms))?;
        let shifted = mix::MixClip {
            dub_start_ms: clip.dub_start_ms - window_start,
            src_start_ms: (clip.src_start_ms - window_start).max(0),
            src_end_ms: (clip.src_end_ms - window_start).max(0),
            ..clip.clone()
        };
        let options = MixOptions {
            kind: if solo {
                ExportKind::DubStem
            } else {
                ExportKind::Mix
            },
            ..MixOptions::default()
        };
        mix::render(&crop, &[shifted], &output, &options, &NoopSink)?;
        Ok(())
    })();
    let _ = std::fs::remove_file(&crop);
    result.map_err(preview_err)?;
    cleanup_previews(&dir, MAX_PREVIEW_FILES, &output);
    let duration_ms = mix::wav_duration_ms(&output).map_err(preview_err)?;
    {
        let guard = tts::lock(db)?;
        guard
            .connection()
            .execute(
                "UPDATE segments SET dub_pushed_ms=0 WHERE id=?1",
                [segment_id],
            )
            .map_err(dberr)?;
    }
    Ok(SegmentPreview {
        segment_id,
        path: output.to_string_lossy().into_owned(),
        window_start_ms: window_start,
        window_end_ms: window_end,
        dub_start_ms: placed.dub_start_ms,
        dub_end_ms: placed.dub_end_ms,
        dub_offset_ms: placed.dub_start_ms - window_start,
        solo,
        spatial: spatial_used,
        duration_ms,
    })
}

// ----- Tauri commands -----

/// Regenerates one Segment as a `regenerate` job (new take → spatialize →
/// replan downstream). `voice_profile_id` defaults to the active take's
/// profile. Returns the `RegenerateOutcome` JSON.
#[tauri::command]
pub async fn regenerate_segment(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
    jobs_state: tauri::State<'_, crate::jobs::JobsState>,
    segment_id: i64,
    expected_revision: i64,
    voice_profile_id: Option<String>,
) -> Result<Value, String> {
    let (media_id, voice_profile_id) = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let segment = db
            .get_segment(segment_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| not_found("segment not found").to_string())?;
        let voice = match voice_profile_id.filter(|id| !id.trim().is_empty()) {
            Some(id) => id,
            None => {
                default_voice_profile(db.connection(), segment_id).map_err(|e| e.to_string())?
            }
        };
        (segment.media_id, voice)
    };
    let input = serde_json::to_value(RegenerateInput {
        segment_id,
        expected_revision,
        voice_profile_id,
    })
    .map_err(|e| e.to_string())?;
    crate::jobs::run_to_completion(
        std::sync::Arc::clone(&jobs_state.0),
        NewJob::new(REGENERATE_JOB, media_id)
            .segment(segment_id)
            .input(input),
        regenerate_runner(app.clone()),
    )
    .await
}

#[tauri::command]
pub fn list_takes(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
    segment_id: i64,
) -> Result<Vec<TakeInfo>, String> {
    let store = tts::asset_store(&app)?;
    let db = state.db.lock().map_err(|e| e.to_string())?;
    takes::list_takes(db.connection(), &store, segment_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_active_take(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
    segment_id: i64,
    take_key: String,
    expected_revision: i64,
) -> Result<ActiveTakeOutcome, String> {
    let store = tts::asset_store(&app)?;
    let db = state.db.lock().map_err(|e| e.to_string())?;
    activate_take(&db, &store, segment_id, &take_key, expected_revision).map_err(|e| e.to_string())
}

/// Clears the pushed flag (UI yellow mark) of the given Segments.
#[tauri::command]
pub fn acknowledge_pushed(
    state: tauri::State<'_, crate::AppState>,
    segment_ids: Vec<i64>,
) -> Result<Vec<Segment>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    clear_pushed(&db, &segment_ids).map_err(|e| e.to_string())
}

/// Renders the context preview WAV (not a job; runs on a blocking thread).
#[tauri::command]
pub async fn preview_segment_context(
    app: tauri::AppHandle,
    segment_id: i64,
    pre_roll_ms: Option<i64>,
    solo: Option<bool>,
    spatial_mode: Option<SpatialMode>,
) -> Result<SegmentPreview, String> {
    let store = tts::asset_store(&app)?;
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let state = app.state::<crate::AppState>();
        render_preview(
            &state.db,
            &store,
            segment_id,
            pre_roll_ms.unwrap_or(DEFAULT_PRE_ROLL_MS),
            solo.unwrap_or(false),
            spatial_mode.unwrap_or_default(),
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests;
