use super::*;
use crate::jobs::{Job, JobStatus, JobStore};
use crate::subtitle::SubtitleCue;
use crate::tts::{MockTts, TtsRequest, MOCK_ADAPTER_VERSION};
use crate::voice::{self, VoiceProfileInput};
use sha2::{Digest, Sha256};
use std::future::Future;
use std::sync::Arc;

struct Fixture {
    db: Mutex<MediaDb>,
    store: AssetStore,
    root: PathBufGuard,
    media_id: i64,
    media_path: std::path::PathBuf,
    ids: Vec<i64>,
    profile: VoiceProfile,
}

struct PathBufGuard(std::path::PathBuf);
impl Drop for PathBufGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

const MEDIA_RATE: u32 = 24_000;

fn write_media(path: &Path, seconds: u32) {
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: MEDIA_RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    for _ in 0..MEDIA_RATE * seconds {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        writer
            .write_sample(((state % 2001) as i32 - 1000) as i16)
            .unwrap();
    }
    writer.finalize().unwrap();
}

/// Media of `seconds` with one Segment per `(start, end, translation)`.
fn fixture(cues: &[(i64, i64, &str)], seconds: u32) -> Fixture {
    let root = std::env::temp_dir().join(format!("tsubame-regen-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let media_path = root.join("media.wav");
    write_media(&media_path, seconds);
    let mut db = MediaDb::open(Path::new(":memory:")).unwrap();
    let media = db.import_media(media_path.to_str().unwrap()).unwrap();
    let rows = db
        .replace_segments(
            media.id,
            &cues
                .iter()
                .map(|(start, end, _)| SubtitleCue {
                    start_ms: *start,
                    end_ms: *end,
                    text: "原文".into(),
                })
                .collect::<Vec<_>>(),
            "import:srt",
        )
        .unwrap();
    let mut ids = Vec::new();
    for (row, (_, _, text)) in rows.iter().zip(cues) {
        let updated = db
            .update_segment(
                row.id,
                row.revision,
                &row.source_text,
                text,
                row.start_ms,
                row.end_ms,
            )
            .unwrap()
            .unwrap();
        ids.push(updated.id);
    }
    let profile = voice::save(
        db.connection(),
        &VoiceProfileInput {
            id: None,
            name: "preset".into(),
            kind: "preset".into(),
            provider_id: "prov".into(),
            model_id: "tts-1".into(),
            preset_voice: Some("alloy".into()),
            design_prompt: None,
            language: "zh-Hans".into(),
            params_json: None,
        },
    )
    .unwrap();
    Fixture {
        store: AssetStore::new(root.join("assets")),
        db: Mutex::new(db),
        media_path,
        root: PathBufGuard(root),
        media_id: media.id,
        ids,
        profile,
    }
}

fn segment(fx: &Fixture, id: i64) -> Segment {
    fx.db.lock().unwrap().get_segment(id).unwrap().unwrap()
}

fn set_translation(fx: &Fixture, id: i64, text: &str) -> Segment {
    let seg = segment(fx, id);
    fx.db
        .lock()
        .unwrap()
        .update_segment(
            id,
            seg.revision,
            &seg.source_text,
            text,
            seg.start_ms,
            seg.end_ms,
        )
        .unwrap()
        .unwrap()
}

/// PR #10 path: `synthesize_segment` with an explicit take.
fn synth(fx: &Fixture, id: i64, take: u32) -> tts::SynthesisOutcome {
    let seg = segment(fx, id);
    tauri::async_runtime::block_on(tts::run_synthesis(
        &fx.db,
        &fx.store,
        &MockTts::new(),
        id,
        seg.revision,
        &fx.profile,
        take,
        &NoopSink,
    ))
    .unwrap()
}

fn no_spatial() -> SpatialAvailability {
    SpatialAvailability::Unavailable("spatial_worker_unavailable: test".into())
}

/// Runs a regenerate job through the job store; returns the terminal row and
/// the runner's result.
fn regen_with<A: TtsAdapter>(
    fx: &Fixture,
    jobs: &Arc<JobStore>,
    id: i64,
    adapter: &A,
    on_created: impl FnOnce(&str),
) -> (Job, Result<RegenerateOutcome, JobError>) {
    let seg = segment(fx, id);
    let input = RegenerateInput {
        segment_id: id,
        expected_revision: seg.revision,
        voice_profile_id: fx.profile.id.clone(),
        generation: None,
        spatial_mode: SpatialMode::SourceTrajectory,
    };
    let job_id = jobs
        .create(
            NewJob::new(REGENERATE_JOB, fx.media_id)
                .segment(id)
                .input(serde_json::to_value(&input).unwrap()),
        )
        .unwrap()
        .id;
    on_created(&job_id);
    let slot: Mutex<Option<Result<RegenerateOutcome, JobError>>> = Mutex::new(None);
    let job = jobs
        .execute(&job_id, &|handle: &JobHandle| {
            let result = run_regenerate(
                &fx.db,
                &fx.store,
                adapter,
                &fx.profile,
                &no_spatial(),
                handle,
                &input,
            );
            *slot.lock().unwrap() = Some(result.clone());
            result.map(|outcome| serde_json::to_value(outcome).unwrap())
        })
        .unwrap();
    (job, slot.into_inner().unwrap().unwrap())
}

fn regen(fx: &Fixture, id: i64) -> RegenerateOutcome {
    let jobs = JobStore::open_in_memory().unwrap();
    let (job, result) = regen_with(fx, &jobs, id, &MockTts::new(), |_| {});
    assert_eq!(job.status, JobStatus::Succeeded, "{:?}", job.error_message);
    result.unwrap()
}

fn take_rows(fx: &Fixture, id: i64) -> Vec<takes::TakeRow> {
    takes::takes_for_segment(fx.db.lock().unwrap().connection(), id).unwrap()
}

// ----- take keys -----

/// Independent re-implementation of the PR #10 key layout (domain tag +
/// length-prefixed fields, `take` rendered as a decimal string).
fn pr10_key(fields: &[&str]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"tsubame.speech.tts.key.v1");
    for field in fields {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn take_zero_key_is_the_pr10_key_and_takes_differ() {
    let input = |take| tts::CacheKeyInput {
        provider_id: "prov",
        model_id: "tts-1",
        adapter_version: MOCK_ADAPTER_VERSION,
        normalized_text: "晚安。",
        language: "zh-Hans",
        voice_profile_hash: "vh",
        params_json: "{}",
        take,
    };
    let fields = [
        "prov",
        "tts-1",
        MOCK_ADAPTER_VERSION,
        "晚安。",
        "zh-Hans",
        "vh",
        "{}",
    ];
    let mut with_zero = fields.to_vec();
    with_zero.push("0");
    assert_eq!(tts::cache_key(&input(0)), pr10_key(&with_zero));
    let mut with_two = fields.to_vec();
    with_two.push("2");
    assert_eq!(tts::cache_key(&input(2)), pr10_key(&with_two));
    assert_ne!(tts::cache_key(&input(1)), tts::cache_key(&input(0)));
    assert_ne!(tts::cache_key(&input(1)), tts::cache_key(&input(2)));
}

#[test]
fn regenerate_creates_next_take_and_lists_takes() {
    let fx = fixture(&[(1_000, 2_000, "晚安。")], 6);
    let first = synth(&fx, fx.ids[0], 0);
    let outcome = regen(&fx, fx.ids[0]);
    assert_eq!(outcome.take_index, 1);
    assert!(!outcome.cache_hit);
    assert_eq!(
        outcome.previous_tts_asset_key.as_deref(),
        Some(first.asset.key.as_str())
    );
    let new_key = outcome.segment.tts_asset_key.clone().unwrap();
    assert_ne!(new_key, first.asset.key);
    assert!(!outcome.segment.dirty_tts && outcome.segment.dirty_mix);
    assert_eq!(outcome.spatial.status, SPATIAL_SKIPPED);
    assert!(outcome
        .spatial
        .reason
        .as_deref()
        .unwrap()
        .starts_with("spatial_worker_unavailable"));
    let listed =
        takes::list_takes(fx.db.lock().unwrap().connection(), &fx.store, fx.ids[0]).unwrap();
    assert_eq!(
        listed.iter().map(|t| t.take_index).collect::<Vec<_>>(),
        vec![1, 0]
    );
    assert!(listed[0].active && !listed[1].active);
    assert!(listed.iter().all(|t| t.available && t.text == "晚安。"));
    assert_eq!(outcome.take.unwrap().tts_asset_key, new_key);
}

#[test]
fn regenerate_without_a_clip_creates_take_zero() {
    // The Inspector's single "Synthesize" action goes through regenerate even
    // when the sentence has never been synthesized.
    let fx = fixture(&[(1_000, 2_000, "晚安。")], 6);
    let id = fx.ids[0];
    assert!(segment(&fx, id).tts_asset_key.is_none());
    assert!(take_rows(&fx, id).is_empty());
    let outcome = regen(&fx, id);
    assert_eq!(outcome.take_index, 0);
    assert!(!outcome.cache_hit);
    assert_eq!(outcome.previous_tts_asset_key, None);
    let key = outcome.segment.tts_asset_key.clone().unwrap();
    assert!(!outcome.segment.dirty_tts);
    let listed = takes::list_takes(fx.db.lock().unwrap().connection(), &fx.store, id).unwrap();
    assert_eq!(listed.len(), 1);
    assert!(listed[0].active && listed[0].available);
    assert_eq!(listed[0].tts_asset_key, key);
    assert_eq!(listed[0].voice_profile_id, fx.profile.id);
    // Take 0 is the PR #11 clip: synthesizing take 0 afterwards reuses it.
    let again = synth(&fx, id, 0);
    assert!(again.cache_hit);
    assert_eq!(again.asset.key, key);
}

#[test]
fn regenerate_refuses_stale_translation_and_stale_revision() {
    let fx = fixture(&[(1_000, 2_000, "晚安。")], 6);
    // Source edit → translation stale, never re-translated automatically.
    let seg = segment(&fx, fx.ids[0]);
    let edited = fx
        .db
        .lock()
        .unwrap()
        .update_segment(
            seg.id,
            seg.revision,
            "新しい原文",
            &seg.translated_text,
            seg.start_ms,
            seg.end_ms,
        )
        .unwrap()
        .unwrap();
    assert!(edited.dirty_translation && edited.dirty_tts);
    assert_eq!(edited.translated_text, "晚安。");
    let jobs = JobStore::open_in_memory().unwrap();
    let (job, result) = regen_with(&fx, &jobs, fx.ids[0], &MockTts::new(), |_| {});
    assert_eq!(job.status, JobStatus::Failed);
    assert_eq!(result.unwrap_err().code, "invalid_input");

    let fx = fixture(&[(1_000, 2_000, "晚安。")], 6);
    let seg = segment(&fx, fx.ids[0]);
    let input = RegenerateInput {
        segment_id: seg.id,
        expected_revision: seg.revision - 1,
        voice_profile_id: fx.profile.id.clone(),
        generation: None,
        spatial_mode: SpatialMode::SourceTrajectory,
    };
    let id = jobs
        .create(NewJob::new(REGENERATE_JOB, fx.media_id))
        .unwrap()
        .id;
    let handle = jobs.start(&id).unwrap();
    let err = run_regenerate(
        &fx.db,
        &fx.store,
        &MockTts::new(),
        &fx.profile,
        &no_spatial(),
        &handle,
        &input,
    )
    .unwrap_err();
    assert_eq!(err.code, "revision_conflict");
    assert!(err.message.contains("segment revision conflict"));
    handle.fail(&err.code, false, &err.message).unwrap();
    assert!(take_rows(&fx, seg.id).is_empty());
}

// ----- pruning -----

#[test]
fn pruning_keeps_five_takes_and_shared_assets() {
    let fx = fixture(&[(1_000, 2_000, "同じ"), (5_000, 6_000, "同じ")], 10);
    let (a, b) = (fx.ids[0], fx.ids[1]);
    let a0 = synth(&fx, a, 0).asset;
    // B only ever has take 1: its key equals A's take-1 key (cache sharing).
    let b1 = synth(&fx, b, 1).asset;
    let mut last = None;
    for _ in 0..6 {
        last = Some(regen(&fx, a));
    }
    let last = last.unwrap();
    let rows = take_rows(&fx, a);
    assert_eq!(
        rows.iter().map(|r| r.take_index).collect::<Vec<_>>(),
        vec![6, 5, 4, 3, 2]
    );
    assert_eq!(last.take_index, 6);
    // Take 0's asset is unreferenced → row + file deleted.
    let conn_guard = fx.db.lock().unwrap();
    let conn = conn_guard.connection();
    assert!(tts::get_asset(conn, &a0.key).unwrap().is_none());
    assert!(!fx.store.absolute(&a0.path).unwrap().exists());
    // Take 1's asset is still B's active clip → kept.
    assert!(tts::get_asset(conn, &b1.key).unwrap().is_some());
    assert!(fx.store.absolute(&b1.path).unwrap().exists());
    assert_eq!(
        conn_guard
            .get_segment(b)
            .unwrap()
            .unwrap()
            .tts_asset_key
            .as_deref(),
        Some(b1.key.as_str())
    );
}

#[test]
fn pruning_never_removes_the_active_take() {
    let fx = fixture(&[(1_000, 2_000, "晚安。")], 6);
    let a = fx.ids[0];
    let guard = fx.db.lock().unwrap();
    let conn = guard.connection();
    for i in 0..7 {
        conn.execute(
            "INSERT INTO tts_takes(segment_id,take_index,tts_asset_key) VALUES(?1,?2,?3)",
            params![a, i, format!("{:064x}", i + 1)],
        )
        .unwrap();
    }
    conn.execute(
        "UPDATE segments SET tts_asset_key=?1 WHERE id=?2",
        params![format!("{:064x}", 1), a],
    )
    .unwrap();
    let result = takes::prune(conn, a).unwrap();
    // Five in total: the (old) active take 0 plus the newest four others.
    assert_eq!(result.pruned_takes, vec![2, 1]);
    let rows = takes::takes_for_segment(conn, a).unwrap();
    assert_eq!(
        rows.iter().map(|r| r.take_index).collect::<Vec<_>>(),
        vec![6, 5, 4, 3, 0]
    );
    assert_eq!(rows.len(), takes::MAX_TAKES_PER_SEGMENT);

    // When the active take is among the newest five, those five are kept.
    conn.execute(
        "UPDATE segments SET tts_asset_key=?1 WHERE id=?2",
        params![format!("{:064x}", 7), a],
    )
    .unwrap();
    let result = takes::prune(conn, a).unwrap();
    assert!(result.pruned_takes.is_empty());
    assert_eq!(takes::takes_for_segment(conn, a).unwrap().len(), 5);
}

// ----- content dedupe -----

fn deterministic() -> MockTts {
    let mut mock = MockTts::new();
    mock.deterministic = true;
    mock
}

fn regen_using<A: TtsAdapter>(fx: &Fixture, id: i64, adapter: &A) -> RegenerateOutcome {
    let jobs = JobStore::open_in_memory().unwrap();
    let (job, result) = regen_with(fx, &jobs, id, adapter, |_| {});
    assert_eq!(job.status, JobStatus::Succeeded, "{:?}", job.error_message);
    result.unwrap()
}

/// Every `.wav` under the TTS asset store.
fn tts_files(fx: &Fixture) -> Vec<std::path::PathBuf> {
    fn walk(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
        let Ok(entries) = std::fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                walk(&path, out);
            } else if path.extension().is_some_and(|e| e == "wav") {
                out.push(path);
            }
        }
    }
    let mut out = Vec::new();
    walk(&fx.store.root.join("tts"), &mut out);
    out
}

#[test]
fn identical_audio_is_not_stored_or_listed_twice() {
    let fx = fixture(&[(1_000, 2_000, "晚安。")], 6);
    let a = fx.ids[0];
    let mock = deterministic();
    let first = regen_using(&fx, a, &mock);
    assert!(first.new_take);
    assert_eq!(first.take_index, 0);
    let rev = segment(&fx, a).revision;

    // Nothing changed and the provider returns the same audio: no new take,
    // no second file, the Segment is not rewritten.
    let again = regen_using(&fx, a, &mock);
    assert_eq!(
        mock.call_count(),
        2,
        "a regenerate always asks the provider"
    );
    assert!(!again.new_take);
    assert_eq!(again.duplicate_of_take, Some(0));
    assert!(again.shared_asset);
    assert_eq!(again.take_index, 0);
    assert_eq!(take_rows(&fx, a).len(), 1);
    assert_eq!(tts_files(&fx).len(), 1);
    assert_eq!(segment(&fx, a).revision, rev);
    assert_eq!(again.plan_change.moved.len(), 0);

    // A different text is a new take; back to the first text the provider
    // returns take 0's audio again → take 0 becomes active, nothing is added.
    set_translation(&fx, a, "晚安，好梦。");
    let second = regen_using(&fx, a, &mock);
    assert!(second.new_take);
    assert_eq!(take_rows(&fx, a).len(), 2);
    set_translation(&fx, a, "晚安。");
    let back = regen_using(&fx, a, &mock);
    assert!(!back.new_take);
    assert_eq!(back.duplicate_of_take, Some(0));
    assert_eq!(
        segment(&fx, a).tts_asset_key.as_deref(),
        Some(first.take.as_ref().unwrap().tts_asset_key.as_str())
    );
    assert!(!segment(&fx, a).dirty_tts);
    assert_eq!(take_rows(&fx, a).len(), 2);
    assert_eq!(tts_files(&fx).len(), 2);
}

#[test]
fn identical_audio_of_another_sentence_shares_its_asset() {
    let fx = fixture(&[(1_000, 2_000, "同じ"), (5_000, 6_000, "別")], 10);
    let (a, b) = (fx.ids[0], fx.ids[1]);
    let mock = deterministic();
    let a0 = regen_using(&fx, a, &mock);
    regen_using(&fx, b, &mock); // B take 0: other text
    set_translation(&fx, b, "同じ");
    let b1 = regen_using(&fx, b, &mock); // B take 1: A's text → A's audio
    assert!(b1.new_take);
    assert!(b1.shared_asset);
    assert_eq!(b1.take_index, 1);
    let a_key = a0.take.unwrap().tts_asset_key;
    assert_eq!(
        segment(&fx, b).tts_asset_key.as_deref(),
        Some(a_key.as_str())
    );
    assert_eq!(tts_files(&fx).len(), 2, "A's file is shared, not copied");
    // The shared asset survives while B still uses it.
    let guard = fx.db.lock().unwrap();
    assert!(takes::key_is_referenced(guard.connection(), &a_key).unwrap());
}

// ----- stale jobs (generation token) -----

/// Runs a regenerate job for an explicit input.
fn run_input<A: TtsAdapter>(
    fx: &Fixture,
    input: &RegenerateInput,
    adapter: &A,
) -> Result<RegenerateOutcome, JobError> {
    let jobs = JobStore::open_in_memory().unwrap();
    let job_id = jobs
        .create(
            NewJob::new(REGENERATE_JOB, fx.media_id)
                .segment(input.segment_id)
                .input(serde_json::to_value(input).unwrap()),
        )
        .unwrap()
        .id;
    let slot: Mutex<Option<Result<RegenerateOutcome, JobError>>> = Mutex::new(None);
    jobs.execute(&job_id, &|handle: &JobHandle| {
        let result = run_regenerate(
            &fx.db,
            &fx.store,
            adapter,
            &fx.profile,
            &no_spatial(),
            handle,
            input,
        );
        *slot.lock().unwrap() = Some(result.clone());
        result.map(|outcome| serde_json::to_value(outcome).unwrap())
    })
    .unwrap();
    slot.into_inner().unwrap().unwrap()
}

/// A request as the `regenerate_segment` command makes it (bumps the token).
fn request(fx: &Fixture, id: i64) -> RegenerateInput {
    let seg = segment(fx, id);
    let generation = takes::bump_generation(fx.db.lock().unwrap().connection(), id).unwrap();
    RegenerateInput {
        segment_id: id,
        expected_revision: seg.revision,
        voice_profile_id: fx.profile.id.clone(),
        generation: Some(generation),
        spatial_mode: SpatialMode::SourceTrajectory,
    }
}

/// Runs `during` while the provider is synthesizing (the job is in flight).
struct ActsDuringTts<'a> {
    inner: MockTts,
    during: Mutex<Option<Box<dyn FnOnce() + Send + 'a>>>,
}

impl TtsAdapter for ActsDuringTts<'_> {
    fn adapter_version(&self) -> &'static str {
        MOCK_ADAPTER_VERSION
    }
    fn synthesize<'b>(
        &'b self,
        request: &'b TtsRequest,
        sink: &'b dyn crate::jobs::ProgressSink,
    ) -> impl Future<Output = Result<Vec<u8>, TtsError>> + Send + 'b {
        if let Some(during) = self.during.lock().unwrap().take() {
            during();
        }
        self.inner.synthesize(request, sink)
    }
}

#[test]
fn a_newer_request_supersedes_an_older_job_in_flight() {
    let fx = fixture(&[(1_000, 2_000, "晚安。")], 6);
    let a = fx.ids[0];
    regen(&fx, a);
    let before = segment(&fx, a);
    let old = request(&fx, a);
    // While the old job synthesizes, the user starts another regenerate.
    let newer: Mutex<Option<RegenerateInput>> = Mutex::new(None);
    let adapter = ActsDuringTts {
        inner: MockTts::new(),
        during: Mutex::new(Some(Box::new(|| {
            *newer.lock().unwrap() = Some(request(&fx, a));
        }))),
    };
    let error = run_input(&fx, &old, &adapter).unwrap_err();
    assert_eq!(error.code, "superseded");
    assert!(!error.retryable);
    // The stale job changed nothing the user can see.
    let after = segment(&fx, a);
    assert_eq!(after.tts_asset_key, before.tts_asset_key);
    assert_eq!(after.revision, before.revision);
    assert_eq!(take_rows(&fx, a).len(), 1);
    // The newer request still applies.
    drop(adapter);
    let newer = newer.into_inner().unwrap().unwrap();
    let outcome = run_input(&fx, &newer, &MockTts::new()).unwrap();
    assert!(outcome.new_take);
    assert_eq!(take_rows(&fx, a).len(), 2);
}

#[test]
fn a_take_selection_supersedes_a_regenerate_in_flight() {
    let fx = fixture(&[(1_000, 2_000, "晚安。")], 6);
    let a = fx.ids[0];
    let take0 = regen(&fx, a).take.unwrap().tts_asset_key;
    regen(&fx, a);
    let mut stale = request(&fx, a);
    // The user switches back to take 0 while the job runs.
    let switched = {
        let guard = fx.db.lock().unwrap();
        activate_take(&guard, &fx.store, a, &take0, stale.expected_revision).unwrap()
    };
    // Even with a revision that matches the new state, the token refuses.
    stale.expected_revision = switched.segment.revision;
    let error = run_input(&fx, &stale, &MockTts::new()).unwrap_err();
    assert_eq!(error.code, "superseded");
    assert_eq!(
        segment(&fx, a).tts_asset_key.as_deref(),
        Some(take0.as_str())
    );
    assert_eq!(take_rows(&fx, a).len(), 2);
}

// ----- set_active_take -----

#[test]
fn set_active_take_is_revision_checked_and_recomputes_dirty_tts() {
    let fx = fixture(&[(1_000, 2_000, "晚安。")], 6);
    let a = fx.ids[0];
    let take0 = synth(&fx, a, 0).asset.key;
    let regenerated = regen(&fx, a);
    let rev = regenerated.segment.revision;

    let guard = fx.db.lock().unwrap();
    let stale = activate_take(&guard, &fx.store, a, &take0, rev - 1).unwrap_err();
    assert_eq!(stale.code, "revision_conflict");
    assert!(stale.to_string().contains("segment revision conflict"));
    let unknown = activate_take(&guard, &fx.store, a, &"f".repeat(64), rev).unwrap_err();
    assert_eq!(unknown.code, "not_found");

    let switched = activate_take(&guard, &fx.store, a, &take0, rev).unwrap();
    assert_eq!(
        switched.segment.tts_asset_key.as_deref(),
        Some(take0.as_str())
    );
    assert_eq!(switched.segment.revision, rev + 1);
    assert!(!switched.segment.dirty_tts && switched.segment.dirty_mix);
    assert!(switched.take.unwrap().active);
    // Same take again: no-op, still revision-checked.
    let same = activate_take(&guard, &fx.store, a, &take0, rev + 1).unwrap();
    assert_eq!(same.segment.revision, rev + 1);
    assert!(activate_take(&guard, &fx.store, a, &take0, rev).is_err());
    drop(guard);

    // A take spoken for an older translation is stale against the new one.
    let edited = set_translation(&fx, a, "晚安，好梦。");
    let guard = fx.db.lock().unwrap();
    let back = activate_take(
        &guard,
        &fx.store,
        a,
        regenerated.segment.tts_asset_key.as_deref().unwrap(),
        edited.revision,
    )
    .unwrap();
    assert!(back.segment.dirty_tts);

    // A missing file is reported, not silently used.
    let asset = tts::get_asset(guard.connection(), &take0).unwrap().unwrap();
    std::fs::remove_file(fx.store.absolute(&asset.path).unwrap()).unwrap();
    let missing = activate_take(&guard, &fx.store, a, &take0, back.segment.revision).unwrap_err();
    assert_eq!(missing.code, "asset_missing");
}

// ----- replan -----

#[test]
fn replan_touches_only_downstream_and_flags_pushed() {
    // Close sentences chain: A ends 1000, B 1100–1300, C 1400–1600, D far away.
    let fx = fixture(
        &[
            (0, 1_000, "一二"),
            (1_100, 1_300, "三四"),
            (1_400, 1_600, "五六"),
            (30_000, 31_000, "七八"),
        ],
        40,
    );
    for id in &fx.ids {
        synth(&fx, *id, 0);
    }
    let before =
        placement::plan_dub(fx.db.lock().unwrap().connection(), fx.media_id, None).unwrap();
    let (a, b, c, d) = (fx.ids[0], fx.ids[1], fx.ids[2], fx.ids[3]);

    // Longer translation for B → longer clip → C is pushed later.
    set_translation(&fx, b, "三四五六七八九十一二三四五");
    let outcome = regen(&fx, b);
    let change = &outcome.plan_change;
    assert_eq!(change.index, 1);
    assert_eq!(change.recomputed_start, 1);
    assert!(change
        .moved
        .iter()
        .all(|s| s.segment_id != a && s.segment_id != b));
    assert_eq!(change.pushed.len(), 1);
    let pushed = &change.pushed[0];
    assert_eq!(pushed.segment_id, c);
    assert!(pushed.delta_ms > 0);
    assert_eq!(pushed.old_start_ms, before.placements[2].dub_start_ms);
    assert!(
        change.moved.iter().all(|s| s.segment_id != d),
        "D is past a natural pause"
    );

    let after = placement::plan_dub(fx.db.lock().unwrap().connection(), fx.media_id, None).unwrap();
    assert_eq!(
        after.placements[0], before.placements[0],
        "upstream untouched"
    );
    assert_eq!(
        after.placements[1].dub_start_ms,
        before.placements[1].dub_start_ms
    );
    assert_eq!(after.placements[2].dub_start_ms, pushed.new_start_ms);
    // Clip durations are never changed by the replan.
    for p in after.placements.iter().filter(|p| p.is_placed()) {
        assert!(p.dub_end_ms > p.dub_start_ms);
    }
    assert_eq!(segment(&fx, c).dub_pushed_ms, pushed.delta_ms);
    assert_eq!(segment(&fx, a).dub_pushed_ms, 0);
    assert_eq!(segment(&fx, b).dub_pushed_ms, 0);
    // The flag is a UI annotation: it does not bump C's revision.
    let c_rev = segment(&fx, c).revision;

    // Shorter again → C moves back earlier and the flag clears.
    set_translation(&fx, b, "三四");
    let back = regen(&fx, b);
    assert!(back.plan_change.pushed.is_empty());
    assert_eq!(back.plan_change.moved[0].segment_id, c);
    assert!(back.plan_change.moved[0].delta_ms < 0);
    assert_eq!(segment(&fx, c).dub_pushed_ms, 0);
    assert_eq!(segment(&fx, c).revision, c_rev);

    // Pushed again, then acknowledged.
    set_translation(&fx, b, "三四五六七八九十一二三四五");
    regen(&fx, b);
    assert!(segment(&fx, c).dub_pushed_ms > 0);
    let acked = clear_pushed(&fx.db.lock().unwrap(), &[c, 9_999]).unwrap();
    assert_eq!(acked.len(), 1);
    assert_eq!(acked[0].dub_pushed_ms, 0);
}

#[test]
fn concurrent_renders_are_made_only_for_shifted_segments_without_one() {
    let fx = fixture(&[(0, 1_000, "一二"), (1_100, 1_300, "三四")], 10);
    for id in &fx.ids {
        synth(&fx, *id, 0);
    }
    let (a, b) = (fx.ids[0], fx.ids[1]);
    let track = "a".repeat(64);
    let guard = fx.db.lock().unwrap();
    let conn = guard.connection();
    let snap = snapshot(conn, fx.media_id, a).unwrap();
    let change = apply_plan_change(conn, &snap, Some(9_000)).unwrap();
    assert_eq!(change.moved.len(), 1);
    let committed = Committed {
        segment: guard.get_segment(a).unwrap().unwrap(),
        previous_key: None,
        change: change.clone(),
        prune: PruneResult::default(),
        snapshot: snap.clone(),
        active_key: snap.rows[0].tts_asset_key.clone().unwrap(),
        take_index: 0,
        new_take: true,
        duplicate_of_take: None,
    };
    let b_row = &snap.rows[1];
    let b_key = b_row.tts_asset_key.clone().unwrap();
    // B moved and has no render under its new concurrent key → render it.
    assert_eq!(
        concurrent_rerender_targets(conn, &fx.store, &track, &committed).unwrap(),
        vec![b]
    );
    let put_render = |key: &str| {
        let rel = spatial::render_relative_path(key).unwrap();
        let path = fx.store.absolute(&rel).unwrap();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"render").unwrap();
        spatial::upsert_asset(
            conn,
            &spatial::SpatialAsset {
                key: key.into(),
                kind: spatial::KIND_RENDER.into(),
                media_id: fx.media_id,
                segment_id: Some(b),
                path: rel,
                sha256: tts::sha256_hex(b"render"),
                created_at: String::new(),
            },
        )
        .unwrap();
    };
    // A render under the old start does not count: the key changed.
    let old_key = concurrent_key(&b_key, &track, b_row, change.moved[0].old_start_ms).unwrap();
    put_render(&old_key);
    assert_eq!(
        concurrent_rerender_targets(conn, &fx.store, &track, &committed).unwrap(),
        vec![b]
    );
    // Already rendered at the new start → nothing to do.
    let new_key = concurrent_key(&b_key, &track, b_row, change.moved[0].new_start_ms).unwrap();
    put_render(&new_key);
    assert!(
        concurrent_rerender_targets(conn, &fx.store, &track, &committed)
            .unwrap()
            .is_empty()
    );
    // A Segment that did not move is never a target (its key is unchanged).
    assert!(committed.change.moved.iter().all(|m| m.segment_id != a));
}

// ----- job progress + cancel -----

#[test]
fn regenerate_job_reports_stages_in_order() {
    let fx = fixture(&[(1_000, 2_000, "晚安。")], 6);
    synth(&fx, fx.ids[0], 0);
    let jobs = JobStore::open_in_memory().unwrap();
    let seen: Arc<Mutex<Vec<(String, i64, i64)>>> = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    jobs.set_listener(Arc::new(move |job: &Job| {
        if !job.stage.is_empty() {
            let mut seen = sink.lock().unwrap();
            let entry = (job.stage.clone(), job.progress_done, job.progress_total);
            if seen.last() != Some(&entry) {
                seen.push(entry);
            }
        }
    }));
    let (job, result) = regen_with(&fx, &jobs, fx.ids[0], &MockTts::new(), |_| {});
    assert_eq!(job.status, JobStatus::Succeeded);
    assert_eq!(job.kind, REGENERATE_JOB);
    assert!(result.is_ok());
    let stages = seen.lock().unwrap().clone();
    let order: Vec<&str> = stages.iter().map(|(s, _, _)| s.as_str()).collect();
    let pos = |name: &str| order.iter().position(|s| *s == name).unwrap();
    assert!(pos(STAGE_TTS) < pos(STAGE_SPATIALIZE));
    assert!(pos(STAGE_SPATIALIZE) < pos(STAGE_REPLAN));
    assert!(pos(STAGE_REPLAN) < pos(STAGE_DONE));
    assert!(stages.iter().all(|(_, _, total)| *total == STAGE_COUNT));
    assert_eq!(stages.last().unwrap(), &(STAGE_DONE.to_string(), 3, 3));
    let output = job.output_json.unwrap();
    assert_eq!(output["spatial"]["status"], SPATIAL_SKIPPED);
    assert_eq!(output["take_index"], 1);
}

/// Cancels its own job while "synthesizing".
struct CancellingTts {
    inner: MockTts,
    jobs: Arc<JobStore>,
    job_id: Mutex<Option<String>>,
}

impl TtsAdapter for CancellingTts {
    fn adapter_version(&self) -> &'static str {
        MOCK_ADAPTER_VERSION
    }

    fn synthesize<'a>(
        &'a self,
        request: &'a TtsRequest,
        _sink: &'a dyn ProgressSink,
    ) -> impl Future<Output = Result<Vec<u8>, TtsError>> + Send + 'a {
        async move {
            let id = self.job_id.lock().unwrap().clone();
            if let Some(id) = id {
                self.jobs.cancel(&id).unwrap();
            }
            self.inner.synthesize(request, &NoopSink).await
        }
    }
}

#[test]
fn cancel_during_tts_commits_nothing() {
    let fx = fixture(&[(1_000, 2_000, "晚安。")], 6);
    let first = synth(&fx, fx.ids[0], 0);
    let before = segment(&fx, fx.ids[0]);
    let jobs = JobStore::open_in_memory().unwrap();
    let adapter = CancellingTts {
        inner: MockTts::new(),
        jobs: Arc::clone(&jobs),
        job_id: Mutex::new(None),
    };
    let (job, result) = regen_with(&fx, &jobs, fx.ids[0], &adapter, |id| {
        *adapter.job_id.lock().unwrap() = Some(id.to_string());
    });
    assert_eq!(job.status, JobStatus::Cancelled);
    assert_eq!(result.unwrap_err().code, "cancelled");
    let after = segment(&fx, fx.ids[0]);
    assert_eq!(after.revision, before.revision);
    assert_eq!(
        after.tts_asset_key.as_deref(),
        Some(first.asset.key.as_str())
    );
    assert_eq!(take_rows(&fx, fx.ids[0]).len(), 1);
}

// ----- preview -----

#[test]
fn preview_window_clamps_and_adds_tail() {
    assert_eq!(
        preview_window(500, 2_000, 1_000),
        (0, 2_000 + PREVIEW_TAIL_MS)
    );
    assert_eq!(preview_window(5_000, 7_000, 1_000), (4_000, 8_000));
    assert_eq!(preview_window(5_000, 7_000, 0), (5_000, 8_000));
    assert_eq!(preview_window(5_000, 7_000, -50), (5_000, 8_000));
}

fn read_i16(path: &Path) -> Vec<i16> {
    hound::WavReader::open(path)
        .unwrap()
        .samples::<i16>()
        .map(|s| s.unwrap())
        .collect()
}

/// The export's options with Spatial follow off (no renders in these fixtures).
fn raw_mix() -> MixOptions {
    MixOptions {
        use_spatial: false,
        ..MixOptions::default()
    }
}

/// What `export_dub_mix` writes for the media with `options`.
fn export_now(fx: &Fixture, options: &MixOptions) -> (Vec<i16>, mix::MixReport) {
    let resolved = mix::resolve_export(&fx.db, &fx.store, fx.media_id, options, &NoopSink).unwrap();
    let out = fx
        .root
        .0
        .join(format!("export-{}.wav", uuid::Uuid::new_v4()));
    let report = mix::render(
        &resolved.inputs.original_path,
        &resolved.inputs.clips,
        &out,
        options,
        &NoopSink,
    )
    .unwrap();
    (read_i16(&out), report)
}

#[test]
fn preview_renders_context_dub_only_and_cleans_up() {
    let fx = fixture(&[(500, 1_500, "晚安。"), (5_000, 6_000, "好梦。")], 10);
    let (a, b) = (fx.ids[0], fx.ids[1]);
    synth(&fx, a, 0);
    let b_clip = synth(&fx, b, 0);
    let plan = placement::plan_dub(fx.db.lock().unwrap().connection(), fx.media_id, None).unwrap();

    // Pre-roll clamped at 0 for A (starts at 500 ms); 1 s tail after the dub.
    let preview = render_preview(&fx.db, &fx.store, a, 1_000, false, &raw_mix()).unwrap();
    assert_eq!(preview.window_start_ms, 0);
    assert_eq!(preview.dub_start_ms, plan.placements[0].dub_start_ms);
    assert_eq!(
        preview.window_end_ms,
        plan.placements[0].dub_end_ms + PREVIEW_TAIL_MS
    );
    assert_eq!(PREVIEW_TAIL_MS, 1_000);
    assert_eq!(preview.dub_offset_ms, preview.dub_start_ms);
    assert!(!preview.spatial && !preview.solo);
    assert_eq!(preview.layered_segment_ids, vec![a]);
    assert_eq!(preview.raw_segment_ids, vec![a]);
    assert!((preview.duration_ms - (preview.window_end_ms - preview.window_start_ms)).abs() <= 1);

    // B: window starts 1000 ms before the sentence; the original part before
    // the dub is bit-identical to the source.
    let preview = render_preview(&fx.db, &fx.store, b, 1_000, false, &raw_mix()).unwrap();
    assert_eq!(preview.window_start_ms, 4_000);
    assert_eq!(
        preview.dub_offset_ms,
        plan.placements[1].dub_start_ms - 4_000
    );
    let original = read_i16(&fx.media_path);
    let rendered = read_i16(Path::new(&preview.path));
    let offset_frames = (preview.dub_offset_ms as usize) * MEDIA_RATE as usize / 1000;
    let start_frame = 4 * MEDIA_RATE as usize;
    assert_eq!(
        rendered[..offset_frames],
        original[start_frame..start_frame + offset_frames]
    );
    assert!(rendered[offset_frames..offset_frames + 2_400]
        .iter()
        .zip(&original[start_frame + offset_frames..])
        .any(|(r, o)| r != o));

    // Dub only: the raw TTS clip of this sentence, as stored.
    let solo = render_preview(&fx.db, &fx.store, b, 1_000, true, &MixOptions::default()).unwrap();
    assert!(solo.solo && !solo.spatial);
    assert_eq!(solo.path, b_clip.path);
    assert_eq!(solo.layered_segment_ids, vec![b]);
    assert_eq!(solo.duration_ms, b_clip.asset.duration_ms);

    // Bounds.
    assert_eq!(
        render_preview(&fx.db, &fx.store, b, MAX_PRE_ROLL_MS + 1, false, &raw_mix())
            .unwrap_err()
            .code,
        "invalid_input"
    );
    // Cleanup: only the newest previews are kept.
    for _ in 0..MAX_PREVIEW_FILES + 3 {
        render_preview(&fx.db, &fx.store, a, 0, false, &raw_mix()).unwrap();
    }
    let names = std::fs::read_dir(fx.store.root.join("preview"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect::<Vec<_>>();
    let count = names.iter().filter(|n| n.starts_with("preview-")).count();
    assert!(count <= MAX_PREVIEW_FILES, "{count} previews kept");
    assert!(count >= 1);
    // No scratch files are left behind.
    assert!(names.iter().all(|n| n.starts_with("preview-")), "{names:?}");
}

#[test]
fn preview_is_the_export_window_bit_for_bit() {
    // A's long dub runs into B's window; B is pushed behind A. A loud dub gain
    // makes the limiter work, so its state at the window start matters.
    let long = "一二三四五六七八九十一二三四五六七八九十一二三四五六七八九十";
    let fx = fixture(&[(1_000, 2_000, long), (4_000, 4_500, "好梦。")], 12);
    let (a, b) = (fx.ids[0], fx.ids[1]);
    synth(&fx, a, 0);
    synth(&fx, b, 0);
    let loud = MixOptions {
        dub_gain_db: 24.0,
        clip_peak_dbfs: None,
        loudness_match: false,
        ..raw_mix()
    };
    for options in [
        raw_mix(),
        loud.clone(),
        MixOptions {
            limiter: false,
            ..loud.clone()
        },
    ] {
        let (export, report) = export_now(&fx, &options);
        if options.limiter && options.dub_gain_db > 0.0 {
            assert!(report.limited_frames > 0, "the limiter engages");
        }
        for id in [a, b] {
            let preview = render_preview(&fx.db, &fx.store, id, 1_000, false, &options).unwrap();
            let rendered = read_i16(Path::new(&preview.path));
            let from = (preview.window_start_ms as usize) * MEDIA_RATE as usize / 1000;
            assert_eq!(
                rendered[..],
                export[from..from + rendered.len()],
                "segment {id} with {options:?}"
            );
        }
    }
}

#[test]
fn preview_layers_every_dub_that_sounds_and_respects_dub_skip() {
    let long = "一二三四五六七八九十一二三四五六七八九十一二三四五六七八九十";
    let fx = fixture(&[(1_000, 2_000, long), (4_000, 4_500, "好梦。")], 12);
    let (a, b) = (fx.ids[0], fx.ids[1]);
    synth(&fx, a, 0);
    synth(&fx, b, 0);
    let plan = placement::plan_dub(fx.db.lock().unwrap().connection(), fx.media_id, None).unwrap();
    let (pa, pb) = (&plan.placements[0], &plan.placements[1]);
    assert!(pa.dub_start_ms < 3_000 && pa.dub_end_ms > 3_000, "{pa:?}");

    // B's window starts at 3000 ms, inside A's dub: A sounds there.
    let preview = render_preview(&fx.db, &fx.store, b, 1_000, false, &raw_mix()).unwrap();
    assert_eq!(preview.window_start_ms, 3_000);
    assert_eq!(preview.layered_segment_ids, vec![a, b]);
    assert_eq!(preview.dub_offset_ms, pb.dub_start_ms - 3_000);
    assert!((preview.duration_ms - (preview.window_end_ms - preview.window_start_ms)).abs() <= 1);

    // A's window reaches into B's dub when B starts before A's tail ends.
    let preview = render_preview(&fx.db, &fx.store, a, 1_000, false, &raw_mix()).unwrap();
    if pb.dub_start_ms < preview.window_end_ms {
        assert_eq!(preview.layered_segment_ids, vec![a, b]);
    } else {
        assert_eq!(preview.layered_segment_ids, vec![a]);
    }

    // dub_skip on A: A is no longer placed, so B's preview carries only B and
    // the part before B's dub is the untouched original.
    fx.db
        .lock()
        .unwrap()
        .connection()
        .execute("UPDATE segments SET dub_skip=1 WHERE id=?1", [a])
        .unwrap();
    let mixed = render_preview(&fx.db, &fx.store, b, 1_000, false, &raw_mix()).unwrap();
    assert_eq!(mixed.layered_segment_ids, vec![b]);
    let original = read_i16(&fx.media_path);
    let rendered = read_i16(Path::new(&mixed.path));
    let start = (mixed.window_start_ms as usize) * MEDIA_RATE as usize / 1000;
    let before = (mixed.dub_offset_ms as usize) * MEDIA_RATE as usize / 1000;
    assert_eq!(rendered[..before], original[start..start + before]);
}

/// Stores `bytes` as a verified spatial render under `key`.
fn put_render(fx: &Fixture, segment_id: i64, key: &str, bytes: &[u8]) {
    let rel = spatial::render_relative_path(key).unwrap();
    let path = fx.store.absolute(&rel).unwrap();
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    spatial::upsert_asset(
        fx.db.lock().unwrap().connection(),
        &spatial::SpatialAsset {
            key: key.into(),
            kind: spatial::KIND_RENDER.into(),
            media_id: fx.media_id,
            segment_id: Some(segment_id),
            path: rel,
            sha256: tts::sha256_hex(bytes),
            created_at: String::new(),
        },
    )
    .unwrap();
}

/// The current `source_trajectory` render key of a Segment's active clip.
fn st_render_key(fx: &Fixture, segment_id: i64) -> String {
    let seg = segment(fx, segment_id);
    let (_, track) = spatial::current_track_key(&fx.media_path).unwrap();
    spatial::render_key(
        seg.tts_asset_key.as_deref().unwrap(),
        &track,
        SpatialMode::SourceTrajectory,
        &RenderWindow {
            src_start_ms: seg.start_ms,
            src_end_ms: seg.end_ms,
            dub_start_ms: 0,
        },
        &Strengths::default(),
        spatial::RENDERER_VERSION,
    )
    .unwrap()
}

#[test]
fn mix_preview_never_falls_back_to_the_raw_clip_silently() {
    let fx = fixture(&[(1_000, 2_000, "晚安。"), (2_100, 2_400, "好梦。")], 8);
    let (a, b) = (fx.ids[0], fx.ids[1]);
    synth(&fx, a, 0);
    let b_clip = synth(&fx, b, 0);
    // Spatial follow on (the export default) and no render of B yet.
    let err =
        render_preview(&fx.db, &fx.store, b, 1_000, false, &MixOptions::default()).unwrap_err();
    assert_eq!(err.code, "spatial_pending");
    assert!(err.retryable);
    // "Dub only" still plays the raw clip, explicitly.
    let solo = render_preview(&fx.db, &fx.store, b, 1_000, true, &MixOptions::default()).unwrap();
    assert!(solo.solo && !solo.spatial);

    // Once B's render exists the mix preview uses it, like the export.
    let render = std::fs::read(&b_clip.path).unwrap();
    put_render(&fx, b, &st_render_key(&fx, b), &render);
    let preview =
        render_preview(&fx.db, &fx.store, b, 1_000, false, &MixOptions::default()).unwrap();
    assert!(preview.spatial);
    assert!(preview.layered_segment_ids.contains(&a));
    // A has no render: mixed raw exactly as the export does, and reported.
    assert_eq!(preview.raw_segment_ids, vec![a]);
}

#[test]
fn preview_clears_pushed_flag_and_needs_a_placed_dub() {
    let fx = fixture(&[(500, 1_500, "晚安。"), (5_000, 6_000, "好梦。")], 10);
    let (a, b) = (fx.ids[0], fx.ids[1]);
    synth(&fx, a, 0);
    let err = render_preview(&fx.db, &fx.store, b, 1_000, false, &raw_mix()).unwrap_err();
    assert_eq!(err.code, "not_placed");
    fx.db
        .lock()
        .unwrap()
        .connection()
        .execute("UPDATE segments SET dub_pushed_ms=420 WHERE id=?1", [a])
        .unwrap();
    render_preview(&fx.db, &fx.store, a, 1_000, false, &raw_mix()).unwrap();
    assert_eq!(segment(&fx, a).dub_pushed_ms, 0);
}

// ----- migration -----

#[test]
fn migration_backfills_take_zero_idempotently() {
    let fx = fixture(&[(1_000, 2_000, "晚安。")], 6);
    let a = fx.ids[0];
    let key = synth(&fx, a, 0).asset.key;
    let path = fx.root.0.join("legacy.db");
    {
        // Build a pre-PR #11 database file: segment points at a clip, no takes table,
        // no dub_pushed_ms column.
        let db = MediaDb::open(&path).unwrap();
        let media = db.import_media(fx.media_path.to_str().unwrap()).unwrap();
        let conn = db.connection();
        conn.execute(
            "INSERT INTO segments(media_id,start_ms,end_ms,translated_text,tts_asset_key)
             VALUES(?1,0,1000,'晚安。',?2)",
            params![media.id, key],
        )
        .unwrap();
        conn.execute_batch("DROP TABLE tts_takes; ALTER TABLE segments DROP COLUMN dub_pushed_ms;")
            .unwrap();
    }
    let reopened = MediaDb::open(&path).unwrap();
    let count = |db: &MediaDb| -> i64 {
        db.connection()
            .query_row("SELECT COUNT(*) FROM tts_takes", [], |r| r.get(0))
            .unwrap()
    };
    assert_eq!(count(&reopened), 1);
    let segment_id: i64 = reopened
        .connection()
        .query_row("SELECT id FROM segments LIMIT 1", [], |r| r.get(0))
        .unwrap();
    let rows = takes::takes_for_segment(reopened.connection(), segment_id).unwrap();
    assert_eq!(rows[0].take_index, 0);
    assert_eq!(rows[0].tts_asset_key, key);
    assert_eq!(rows[0].text, "晚安。");
    assert_eq!(
        reopened
            .get_segment(segment_id)
            .unwrap()
            .unwrap()
            .dub_pushed_ms,
        0
    );
    takes::migrate(reopened.connection()).unwrap();
    drop(reopened);
    let again = MediaDb::open(&path).unwrap();
    assert_eq!(count(&again), 1);
    assert_eq!(
        takes::next_take_index(again.connection(), segment_id).unwrap(),
        1
    );
}

#[test]
fn default_voice_profile_comes_from_the_active_take() {
    let fx = fixture(&[(1_000, 2_000, "晚安。")], 6);
    let a = fx.ids[0];
    let guard = fx.db.lock().unwrap();
    assert_eq!(
        default_voice_profile(guard.connection(), a)
            .unwrap_err()
            .code,
        "invalid_input"
    );
    drop(guard);
    synth(&fx, a, 0);
    assert_eq!(
        default_voice_profile(fx.db.lock().unwrap().connection(), a).unwrap(),
        fx.profile.id
    );
}

// ----- real spatial worker -----

/// `python3` (or `TSUBAME_TEST_PYTHON`) with numpy importable.
fn python_with_numpy() -> Option<String> {
    let python = crate::worker::test_support::python()?;
    std::process::Command::new(&python)
        .args(["-c", "import numpy"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|_| python)
}

/// Three chained sentences over a stereo reference (the spatial analysis wants
/// one), every clip synthesized as take 0, and a worker spec.
fn spatial_fixture(python: String) -> (Fixture, WorkerSpec) {
    let fx = fixture(
        &[
            (0, 1_000, "一二"),
            (1_100, 1_300, "三四"),
            (1_400, 1_600, "五六"),
        ],
        6,
    );
    {
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48_000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(&fx.media_path, spec).unwrap();
        let mut state = 7u64;
        for i in 0..48_000 * 6 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            let v = ((state % 20_001) as f64 / 10_000.0 - 1.0)
                * (0.5 + 0.5 * (i as f64 / 48_000.0 * 3.0).sin());
            writer.write_sample((v * 12_000.0) as i16).unwrap();
            writer.write_sample((v * 4_000.0) as i16).unwrap();
        }
        writer.finalize().unwrap();
    }
    for id in &fx.ids {
        synth(&fx, *id, 0);
    }
    let mut spec = WorkerSpec::python(python, spatial::worker_script());
    spec.hello_timeout = std::time::Duration::from_secs(30);
    (fx, spec)
}

fn spatialize_now(fx: &Fixture, spec: &WorkerSpec, segment_id: i64, mode: SpatialMode) {
    let jobs = JobStore::open_in_memory().unwrap();
    let id = jobs
        .create(NewJob::new(spatial::SPATIAL_JOB, fx.media_id))
        .unwrap()
        .id;
    let handle = jobs.start(&id).unwrap();
    spatial::spatialize(&fx.db, &fx.store, spec, &handle, segment_id, mode)
        .expect("render through the worker");
    handle.succeed(serde_json::json!({})).unwrap();
}

/// Regenerates B (longer text, so C is pushed) with the real worker.
fn regenerate_b_with_worker(
    fx: &Fixture,
    spec: &WorkerSpec,
    mode: SpatialMode,
    adapter: &MockTts,
) -> RegenerateOutcome {
    let b = fx.ids[1];
    set_translation(fx, b, "三四五六七八九十一二三四五");
    let seg = segment(fx, b);
    let input = RegenerateInput {
        segment_id: b,
        expected_revision: seg.revision,
        voice_profile_id: fx.profile.id.clone(),
        generation: None,
        spatial_mode: mode,
    };
    let jobs = JobStore::open_in_memory().unwrap();
    let id = jobs
        .create(NewJob::new(REGENERATE_JOB, fx.media_id).segment(b))
        .unwrap()
        .id;
    let handle = jobs.start(&id).unwrap();
    let outcome = run_regenerate(
        &fx.db,
        &fx.store,
        adapter,
        &fx.profile,
        &SpatialAvailability::Available(spec.clone()),
        &handle,
        &input,
    )
    .unwrap();
    handle.succeed(serde_json::json!({})).unwrap();
    assert_eq!(
        outcome.spatial.status, SPATIAL_DONE,
        "{:?}",
        outcome.spatial
    );
    outcome
}

#[test]
fn source_trajectory_regenerate_spatializes_only_the_new_clip() {
    let Some(python) = python_with_numpy() else {
        eprintln!("skipping: python3 with numpy not available");
        return;
    };
    let (fx, spec) = spatial_fixture(python);
    let (a, b, c) = (fx.ids[0], fx.ids[1], fx.ids[2]);
    spatialize_now(&fx, &spec, c, SpatialMode::SourceTrajectory);
    let mock = MockTts::new();
    let outcome = regenerate_b_with_worker(&fx, &spec, SpatialMode::SourceTrajectory, &mock);
    assert_eq!(mock.call_count(), 1, "only B is synthesized");
    assert!(outcome.plan_change.pushed.iter().any(|s| s.segment_id == c));
    let renders = &outcome.spatial.renders;
    assert_eq!(renders.len(), 1, "{renders:?}");
    assert_eq!(renders[0].segment_id, b);
    assert_eq!(renders[0].mode, SpatialMode::SourceTrajectory);
    assert!(!renders[0].cache_hit);
    assert!(renders
        .iter()
        .all(|r| r.segment_id != a && r.segment_id != c));
}

#[test]
fn concurrent_regenerate_rerenders_only_shifted_segments() {
    let Some(python) = python_with_numpy() else {
        eprintln!("skipping: python3 with numpy not available");
        return;
    };
    let (fx, spec) = spatial_fixture(python);
    let (a, b, c) = (fx.ids[0], fx.ids[1], fx.ids[2]);
    for id in [a, c] {
        spatialize_now(&fx, &spec, id, SpatialMode::Concurrent);
    }
    let mock = MockTts::new();
    let outcome = regenerate_b_with_worker(&fx, &spec, SpatialMode::Concurrent, &mock);
    assert_eq!(
        mock.call_count(),
        1,
        "C is re-rendered, never re-synthesized"
    );
    let renders = &outcome.spatial.renders;
    assert!(renders
        .iter()
        .any(|r| r.segment_id == b && r.mode == SpatialMode::Concurrent && !r.cache_hit));
    assert!(
        renders
            .iter()
            .any(|r| r.segment_id == c && r.mode == SpatialMode::Concurrent && !r.cache_hit),
        "shifted C is re-rendered: {renders:?}"
    );
    assert!(
        renders.iter().all(|r| r.segment_id != a),
        "A (before the edit, not shifted) is untouched: {renders:?}"
    );
    assert!(renders.iter().all(|r| r.mode == SpatialMode::Concurrent));
}
