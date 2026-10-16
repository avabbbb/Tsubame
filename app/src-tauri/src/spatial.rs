//! `audio.spatialize` host side: cache keys, the `spatial_assets` table and the
//! `spatial` job runner that drives the Python worker
//! (`workers/dubber/spatial/worker.py`).
//!
//! Contract: `docs/19-dubbing-pipeline.md` § audio.spatialize and the worker's
//! `workers/dubber/spatial/README.md` (ops, input schema, key derivation).
//! Rust owns state, cache keys and asset moves; the worker writes only inside
//! the `out_dir` it is given.
//!
//! Keys are byte-identical to the Python reference implementations
//! (`analyze.spatial_track_key`, `render.render_key`): SHA-256 over a canonical
//! JSON array (domain tag first, sorted keys, `,`/`:` separators, UTF-8, Python
//! `float.__repr__` for non-integers). The README's pinned vectors are tested here.
//!
//! Flow:
//! - analysis, once per media and cached by `spatial_track_key`: the reference is
//!   the media file itself (`reference_kind: mix`, default params), hashed by the
//!   host;
//! - render, once per Segment clip and cached by `render_key` (TTS asset key,
//!   track key, mode, window, strengths, renderer version);
//! - outputs are promoted into the asset store (`spatial/tracks/…npz`,
//!   `spatial/renders/…wav`) and recorded in `spatial_assets`; a verified cache
//!   row skips the worker entirely.
//! - mix (`MixOptions.use_spatial`, default on) swaps a clip's raw TTS file for
//!   its render when one exists for the clip's *current* render key
//!   ([`apply_spatial_renders`]).

use crate::db::MediaDb;
use crate::jobs::{JobError, JobHandle, JobRunner, NewJob};
use crate::mix::MixClip;
use crate::placement::{self, PlacementParams};
use crate::tts::{self, AssetStore};
use crate::worker::{self, WorkerSpec};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Job kind of `analyze_spatial` / `spatialize_segment` runs.
pub const SPATIAL_JOB: &str = "spatial";
/// Worker capability (protocol v1 `op`).
pub const CAPABILITY: &str = "audio.spatialize";
/// Must equal `spatial.ANALYZER_VERSION` in the worker.
pub const ANALYZER_VERSION: &str = "spatial-analyze/1";
/// Must equal `spatial.RENDERER_VERSION` in the worker.
pub const RENDERER_VERSION: &str = "spatial-render/1";
/// The host analyses the media file itself (whole mix; no vocal stem yet).
pub const REFERENCE_KIND: &str = "mix";

const TRACK_DOMAIN: &str = "tsubame.spatial_track.v1";
const RENDER_DOMAIN: &str = "tsubame.spatial_render.v1";

pub const KIND_ANALYSIS: &str = "analysis";
pub const KIND_RENDER: &str = "render";

// ----- canonical JSON + keys -----

/// Python `repr(float)` (shortest round-trip digits; scientific notation when the
/// decimal exponent is < -4 or >= 16, e.g. `1e-05`, `1e+16`; always a `.0` or
/// fraction otherwise, e.g. `18000.0`).
pub fn py_float_repr(value: f64) -> String {
    // `{:e}` gives the shortest round-trip digits, e.g. "1.8e4", "-9e-1", "0e0".
    let sci = format!("{value:e}");
    let (mantissa, exponent) = sci.split_once('e').expect("{:e} always has an exponent");
    let exponent: i32 = exponent.parse().expect("{:e} exponent is an integer");
    let (negative, mantissa) = match mantissa.strip_prefix('-') {
        Some(rest) => (true, rest),
        None => (false, mantissa),
    };
    let digits: String = mantissa.chars().filter(|c| *c != '.').collect();
    let mut out = String::new();
    if negative {
        out.push('-');
    }
    if (-4..16).contains(&exponent) {
        if exponent >= 0 {
            let int_len = exponent as usize + 1;
            if digits.len() <= int_len {
                out.push_str(&digits);
                out.push_str(&"0".repeat(int_len - digits.len()));
                out.push_str(".0");
            } else {
                out.push_str(&digits[..int_len]);
                out.push('.');
                out.push_str(&digits[int_len..]);
            }
        } else {
            out.push_str("0.");
            out.push_str(&"0".repeat((-exponent - 1) as usize));
            out.push_str(&digits);
        }
    } else {
        out.push_str(&digits[..1]);
        if digits.len() > 1 {
            out.push('.');
            out.push_str(&digits[1..]);
        }
        out.push('e');
        out.push(if exponent < 0 { '-' } else { '+' });
        out.push_str(&format!("{:02}", exponent.unsigned_abs()));
    }
    out
}

fn write_json_string(value: &str, out: &mut String) {
    // `json.dumps(..., ensure_ascii=False)`: escape only `"`, `\` and U+0000..U+001F.
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '\u{08}' => out.push_str("\\b"),
            '\u{0c}' => out.push_str("\\f"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn write_canonical(value: &Value, out: &mut String) -> Result<(), String> {
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        Value::Number(number) => {
            if let Some(int) = number.as_i64() {
                out.push_str(&int.to_string());
            } else if let Some(int) = number.as_u64() {
                out.push_str(&int.to_string());
            } else {
                let float = number
                    .as_f64()
                    .filter(|f| f.is_finite())
                    .ok_or_else(|| "canonical JSON needs finite numbers".to_string())?;
                out.push_str(&py_float_repr(float));
            }
        }
        Value::String(text) => write_json_string(text, out),
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            // Python sorts by code point; UTF-8 byte order is the same order.
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            out.push('{');
            for (i, key) in keys.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_json_string(key, out);
                out.push(':');
                write_canonical(&map[key], out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// `json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)`.
/// JSON integers stay integers; JSON floats use Python's float repr, so callers
/// must build float-typed fields from `f64` (`json!(20.0)`), int-typed from ints.
pub fn canonical_json(value: &Value) -> Result<String, String> {
    let mut out = String::new();
    write_canonical(value, &mut out)?;
    Ok(out)
}

fn sha256_hex(text: &str) -> String {
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Fully resolved default analysis params (`analyze.DEFAULT_PARAMS`), with the
/// worker's int/float typing.
pub fn default_analysis_params() -> Value {
    json!({
        "hop_ms": 20,
        "frame_ms": 85,
        "bands": 28,
        "band_lo_hz": 100.0,
        "band_hi_hz": 18000.0,
        "band_halfwidth_ratio": 1.22,
        "ild_clip_db": 24.0,
        "itd_lo_hz": 180.0,
        "itd_hi_hz": 6500.0,
        "itd_max_ms": 0.9,
        "itd_reliable_coherence": 0.15,
        "activity_floor_db": -85.0,
        "activity_below_p90_db": 30.0,
        "color_clip_db": 6.0,
        "ild_hardpan_db": 12.0,
        "smoothing": {"itd_median": 5, "itd": 7, "ild": 9, "level": 19, "color": 19},
    })
}

/// `sha256(["tsubame.spatial_track.v1", reference_sha256, reference_kind,
/// resolved_params, analyzer_version])`. `resolved_params` must already be
/// resolved against the defaults (see [`default_analysis_params`]).
pub fn spatial_track_key(
    reference_sha256: &str,
    reference_kind: &str,
    resolved_params: &Value,
    analyzer_version: &str,
) -> Result<String, String> {
    let payload = json!([
        TRACK_DOMAIN,
        reference_sha256.to_ascii_lowercase(),
        reference_kind,
        resolved_params,
        analyzer_version
    ]);
    Ok(sha256_hex(&canonical_json(&payload)?))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpatialMode {
    /// The original sentence path stretched to the clip (timing edits do not re-render).
    #[default]
    SourceTrajectory,
    /// The original's position at the dub's play time.
    Concurrent,
}

impl SpatialMode {
    pub fn as_str(self) -> &'static str {
        match self {
            SpatialMode::SourceTrajectory => "source_trajectory",
            SpatialMode::Concurrent => "concurrent",
        }
    }
}

/// Render strengths (`render.DEFAULT_STRENGTHS`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Strengths {
    pub rtf: f64,
    pub level: f64,
    pub color: f64,
}

impl Default for Strengths {
    fn default() -> Self {
        Self {
            rtf: 1.0,
            level: 0.7,
            color: 0.35,
        }
    }
}

impl Strengths {
    fn to_value(self) -> Value {
        json!({"rtf": self.rtf, "level": self.level, "color": self.color})
    }
}

/// Integer-ms render window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderWindow {
    pub src_start_ms: i64,
    pub src_end_ms: i64,
    pub dub_start_ms: i64,
}

impl RenderWindow {
    /// The fields that can change the rendered samples (`render.key_window`):
    /// `dub_start_ms` only in `concurrent` mode.
    pub fn key_value(&self, mode: SpatialMode) -> Value {
        let mut window = json!({
            "src_start_ms": self.src_start_ms,
            "src_end_ms": self.src_end_ms,
        });
        if mode == SpatialMode::Concurrent {
            window["dub_start_ms"] = json!(self.dub_start_ms);
        }
        window
    }
}

/// `sha256(["tsubame.spatial_render.v1", tts_asset_key, spatial_track_key, mode,
/// key_window, strengths, renderer_version])`.
pub fn render_key(
    tts_asset_key: &str,
    spatial_track_key: &str,
    mode: SpatialMode,
    window: &RenderWindow,
    strengths: &Strengths,
    renderer_version: &str,
) -> Result<String, String> {
    let payload = json!([
        RENDER_DOMAIN,
        tts_asset_key,
        spatial_track_key,
        mode.as_str(),
        window.key_value(mode),
        strengths.to_value(),
        renderer_version
    ]);
    Ok(sha256_hex(&canonical_json(&payload)?))
}

// ----- persistence -----

/// Idempotent migration for `spatial_assets`. Called from `MediaDb::open`.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS spatial_assets (
           key TEXT PRIMARY KEY,
           kind TEXT NOT NULL CHECK (kind IN ('analysis','render')),
           media_id INTEGER NOT NULL,
           segment_id INTEGER,
           path TEXT NOT NULL,
           sha256 TEXT NOT NULL,
           created_at TEXT NOT NULL DEFAULT (datetime('now'))
         );
         CREATE INDEX IF NOT EXISTS idx_spatial_assets_media
           ON spatial_assets(media_id, kind);",
    )
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SpatialAsset {
    pub key: String,
    /// `analysis` | `render`.
    pub kind: String,
    pub media_id: i64,
    pub segment_id: Option<i64>,
    /// Relative to the asset store root (`spatial/renders/ab/<key>.wav`).
    pub path: String,
    pub sha256: String,
    pub created_at: String,
}

pub fn get_asset(conn: &Connection, key: &str) -> rusqlite::Result<Option<SpatialAsset>> {
    conn.query_row(
        "SELECT key,kind,media_id,segment_id,path,sha256,created_at
           FROM spatial_assets WHERE key=?1",
        [key],
        |row| {
            Ok(SpatialAsset {
                key: row.get(0)?,
                kind: row.get(1)?,
                media_id: row.get(2)?,
                segment_id: row.get(3)?,
                path: row.get(4)?,
                sha256: row.get(5)?,
                created_at: row.get(6)?,
            })
        },
    )
    .optional()
}

pub fn upsert_asset(conn: &Connection, asset: &SpatialAsset) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO spatial_assets(key,kind,media_id,segment_id,path,sha256)
         VALUES(?1,?2,?3,?4,?5,?6)
         ON CONFLICT(key) DO UPDATE SET
           kind=excluded.kind,media_id=excluded.media_id,segment_id=excluded.segment_id,
           path=excluded.path,sha256=excluded.sha256",
        params![
            asset.key,
            asset.kind,
            asset.media_id,
            asset.segment_id,
            asset.path,
            asset.sha256
        ],
    )?;
    Ok(())
}

/// Whether any render is cached for the media (lets mix skip hashing the original).
pub fn has_renders(conn: &Connection, media_id: i64) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM spatial_assets WHERE media_id=?1 AND kind='render')",
        [media_id],
        |row| row.get::<_, i64>(0),
    )
    .map(|flag| flag != 0)
}

fn check_key(key: &str) -> Result<(), String> {
    if key.len() == 64 && key.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(())
    } else {
        Err("invalid spatial asset key".to_string())
    }
}

/// `spatial/tracks/<k[0..2]>/<key>.npz` (always `/`).
pub fn track_relative_path(key: &str) -> Result<String, String> {
    check_key(key)?;
    Ok(format!("spatial/tracks/{}/{key}.npz", &key[..2]))
}

/// `spatial/renders/<k[0..2]>/<key>.wav` (always `/`).
pub fn render_relative_path(key: &str) -> Result<String, String> {
    check_key(key)?;
    Ok(format!("spatial/renders/{}/{key}.wav", &key[..2]))
}

/// The cached asset of `kind` if its row exists **and** its file is present
/// with the recorded sha256. Anything else is a cache miss.
pub fn verified_asset(
    conn: &Connection,
    store: &AssetStore,
    key: &str,
    kind: &str,
) -> Result<Option<(SpatialAsset, PathBuf)>, String> {
    let Some(asset) = get_asset(conn, key).map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    if asset.kind != kind {
        return Ok(None);
    }
    let path = store.absolute(&asset.path).map_err(|e| e.to_string())?;
    match worker::sha256_file(&path) {
        Ok(sha) if sha == asset.sha256 => Ok(Some((asset, path))),
        _ => Ok(None),
    }
}

// ----- mix selection -----

/// Replaces each clip's path with its spatial render when a verified render
/// exists for the clip's **current** render key (its TTS asset key, the media's
/// current track key, `mode`, its window and default strengths). Clips without
/// one keep the raw TTS file. Returns how many clips were swapped.
pub fn apply_spatial_renders(
    conn: &Connection,
    store: &AssetStore,
    clips: &mut [MixClip],
    spatial_track_key: &str,
    mode: SpatialMode,
) -> Result<usize, String> {
    let mut swapped = 0;
    for clip in clips.iter_mut() {
        let Some(tts_key) = clip.tts_asset_key.as_deref() else {
            continue;
        };
        let window = RenderWindow {
            src_start_ms: clip.src_start_ms,
            src_end_ms: clip.src_end_ms,
            dub_start_ms: clip.dub_start_ms,
        };
        let key = render_key(
            tts_key,
            spatial_track_key,
            mode,
            &window,
            &Strengths::default(),
            RENDERER_VERSION,
        )?;
        if let Some((_, path)) = verified_asset(conn, store, &key, KIND_RENDER)? {
            clip.path = path;
            swapped += 1;
        }
    }
    Ok(swapped)
}

// ----- worker -----

/// Runtime pack that supplies the spatial worker's Python interpreter and numpy,
/// installed under <app data>/runtimes/<id>/current/ (docs/06-runtime-bootstrap.md).
pub const SPATIAL_RUNTIME_ID: &str = "dub-spatial";
/// Worker script, relative to the bundled resource directory (and, in debug
/// builds only, to the repository root).
pub const WORKER_RESOURCE: &str = "workers/dubber/spatial/worker.py";

/// Where the spatial worker and its runtime are looked up. The system python3
/// is never used, and release builds never look at the build machine's checkout.
#[derive(Debug, Clone, Default)]
pub struct WorkerRoots {
    /// Tauri resource directory holding the bundled workers/dubber/ tree.
    pub resource_dir: Option<PathBuf>,
    /// App data directory holding runtimes/<id>/current/.
    pub app_data_dir: Option<PathBuf>,
}

impl WorkerRoots {
    pub fn from_app(app: &tauri::AppHandle) -> Self {
        use tauri::Manager;
        Self {
            resource_dir: app.path().resource_dir().ok(),
            app_data_dir: app.path().app_data_dir().ok(),
        }
    }
}

fn env_path(name: &str) -> Option<PathBuf> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
}

/// Interpreter inside an installed runtime pack directory.
fn runtime_python(pack_dir: &Path) -> PathBuf {
    if cfg!(windows) {
        pack_dir.join("python.exe")
    } else {
        pack_dir.join("bin").join("python3")
    }
}

/// Python interpreter: TSUBAME_PYTHON (development override), else the
/// dub-spatial runtime pack's interpreter when it is installed. None when
/// neither exists; there is no fallback to python3 on PATH.
pub fn python_program(roots: &WorkerRoots) -> Option<PathBuf> {
    if let Some(program) = env_path("TSUBAME_PYTHON") {
        return Some(program);
    }
    let pack = roots
        .app_data_dir
        .as_ref()?
        .join("runtimes")
        .join(SPATIAL_RUNTIME_ID)
        .join("current");
    Some(runtime_python(&pack)).filter(|program| program.is_file())
}

/// Worker script: TSUBAME_SPATIAL_WORKER, else the copy bundled in the app's
/// resources; debug builds (dev runs and tests) also fall back to the checkout.
pub fn worker_script(roots: &WorkerRoots) -> Option<PathBuf> {
    if let Some(script) = env_path("TSUBAME_SPATIAL_WORKER") {
        return Some(script);
    }
    roots
        .resource_dir
        .as_ref()
        .map(|dir| dir.join(WORKER_RESOURCE))
        .filter(|script| script.is_file())
        .or_else(dev_worker_script)
}

#[cfg(debug_assertions)]
fn dev_worker_script() -> Option<PathBuf> {
    Some(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join(WORKER_RESOURCE),
    )
    .filter(|script| script.is_file())
}

#[cfg(not(debug_assertions))]
fn dev_worker_script() -> Option<PathBuf> {
    None
}

/// Spec for the spatial worker, or a runtime_missing reason.
pub fn worker_spec(roots: &WorkerRoots) -> Result<WorkerSpec, String> {
    let program = python_program(roots).ok_or_else(|| {
        format!("runtime_missing: the {SPATIAL_RUNTIME_ID} runtime pack is not installed")
    })?;
    let script = worker_script(roots).ok_or_else(|| {
        "runtime_missing: the bundled spatial worker script was not found".to_string()
    })?;
    Ok(WorkerSpec::python(program, script))
}

// ----- orchestration -----

fn db_err(error: impl std::fmt::Display) -> JobError {
    JobError::new("db", true, error.to_string())
}

fn io_err(error: impl std::fmt::Display) -> JobError {
    JobError::new("io", true, error.to_string())
}

fn invalid(message: impl Into<String>) -> JobError {
    JobError::new("invalid_input", false, message)
}

fn internal(error: impl std::fmt::Display) -> JobError {
    JobError::new("internal", false, error.to_string())
}

fn lock(db: &Mutex<MediaDb>) -> Result<std::sync::MutexGuard<'_, MediaDb>, JobError> {
    db.lock().map_err(db_err)
}

/// Removes a job's scratch directory when dropped (success, failure or cancel).
struct Scratch(PathBuf);

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn scratch_dir(store: &AssetStore, job: &JobHandle, tag: &str) -> Scratch {
    Scratch(
        store
            .root
            .join("spatial")
            .join("tmp")
            .join(format!("{}-{tag}", job.id())),
    )
}

fn output_str<'a>(output: &'a Value, key: &str) -> Result<&'a str, JobError> {
    output.get(key).and_then(Value::as_str).ok_or_else(|| {
        JobError::new(
            worker::codes::PROTOCOL_VIOLATION,
            false,
            format!("worker result has no {key}"),
        )
    })
}

/// Media file path (the analysis reference), made absolute.
fn reference_path(db: &MediaDb, media_id: i64) -> Result<PathBuf, JobError> {
    let path: String = db
        .connection()
        .query_row(
            "SELECT path FROM media_files WHERE id=?1",
            [media_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db_err)?
        .ok_or_else(|| JobError::new("not_found", false, "media not found"))?;
    std::fs::canonicalize(&path).map_err(|e| {
        JobError::new(
            "input_missing",
            false,
            format!("media file is not readable: {e}"),
        )
    })
}

/// Current track key of a media file (hashes the file; call without the DB lock).
pub fn current_track_key(reference: &Path) -> Result<(String, String), JobError> {
    let sha = worker::sha256_file(reference).map_err(|e| {
        JobError::new(
            "input_missing",
            false,
            format!("cannot read media file: {e}"),
        )
    })?;
    let key = spatial_track_key(
        &sha,
        REFERENCE_KIND,
        &default_analysis_params(),
        ANALYZER_VERSION,
    )
    .map_err(internal)?;
    Ok((sha, key))
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct AnalysisOutcome {
    pub media_id: i64,
    pub spatial_track_key: String,
    pub reference_sha256: String,
    /// Absolute path of the cached `spatial_track.npz`.
    pub track_path: String,
    pub cache_hit: bool,
    /// Worker `summary` (only on a fresh analysis).
    pub summary: Option<Value>,
}

/// Analysis for `media_id`, from the cache or by running the worker's `analyze`.
pub fn ensure_analysis(
    db: &Mutex<MediaDb>,
    store: &AssetStore,
    spec: &WorkerSpec,
    job: &JobHandle,
    media_id: i64,
) -> Result<AnalysisOutcome, JobError> {
    let reference = reference_path(&*lock(db)?, media_id)?;
    job.progress(0, 1, "hash");
    let (sha, key) = current_track_key(&reference)?;
    if job.is_cancelled() {
        return Err(JobError::cancelled());
    }
    if let Some((_, path)) =
        verified_asset(lock(db)?.connection(), store, &key, KIND_ANALYSIS).map_err(db_err)?
    {
        return Ok(AnalysisOutcome {
            media_id,
            spatial_track_key: key,
            reference_sha256: sha,
            track_path: path.to_string_lossy().into_owned(),
            cache_hit: true,
            summary: None,
        });
    }
    let scratch = scratch_dir(store, job, "analyze");
    let input = json!({
        "op": "analyze",
        "reference_path": reference.to_string_lossy(),
        "reference_kind": REFERENCE_KIND,
        "reference_sha256": sha,
        "params": {},
    });
    let result = job.run_worker(spec, CAPABILITY, input, vec![reference], scratch.0.clone())?;
    let output = &result.output;
    if output.get("analyzer_version").and_then(Value::as_str) != Some(ANALYZER_VERSION)
        || output.get("spatial_track_key").and_then(Value::as_str) != Some(key.as_str())
    {
        return Err(JobError::new(
            "spatial_key_mismatch",
            false,
            "the spatial worker's analyzer version or track key differs from the host's",
        ));
    }
    let relative = track_relative_path(&key).map_err(internal)?;
    let dest = store.absolute(&relative).map_err(io_err)?;
    worker::promote_output(Path::new(output_str(output, "track_path")?), &dest).map_err(io_err)?;
    if let Ok(summary) = output_str(output, "summary_path") {
        // Human-readable sidecar next to the track; not part of the cache row.
        let _ = worker::promote_output(Path::new(summary), &dest.with_extension("json"));
    }
    let asset = SpatialAsset {
        key: key.clone(),
        kind: KIND_ANALYSIS.to_string(),
        media_id,
        segment_id: None,
        path: relative,
        sha256: worker::sha256_file(&dest).map_err(io_err)?,
        created_at: String::new(),
    };
    upsert_asset(lock(db)?.connection(), &asset).map_err(db_err)?;
    Ok(AnalysisOutcome {
        media_id,
        spatial_track_key: key,
        reference_sha256: sha,
        track_path: dest.to_string_lossy().into_owned(),
        cache_hit: false,
        summary: output.get("summary").cloned(),
    })
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct RenderOutcome {
    pub segment_id: i64,
    pub media_id: i64,
    pub mode: SpatialMode,
    pub render_key: String,
    pub spatial_track_key: String,
    pub tts_asset_key: String,
    /// Absolute path of the stereo render (frontend: `convertFileSrc`).
    pub path: String,
    pub cache_hit: bool,
    pub analysis_cache_hit: bool,
    /// Worker `report` (only on a fresh render).
    pub report: Option<Value>,
}

/// What a render needs, read under the DB lock.
struct RenderPlan {
    media_id: i64,
    reference: PathBuf,
    tts_key: String,
    clip: PathBuf,
    window: RenderWindow,
}

fn plan_render(
    db: &MediaDb,
    store: &AssetStore,
    segment_id: i64,
    mode: SpatialMode,
) -> Result<RenderPlan, JobError> {
    let segment = db
        .get_segment(segment_id)
        .map_err(db_err)?
        .ok_or_else(|| JobError::new("not_found", false, "segment not found"))?;
    if segment.dub_skip {
        return Err(invalid("sentence is marked skip for dubbing"));
    }
    let tts = tts::resolve_segment_tts_asset(db, store, segment_id)?
        .ok_or_else(|| JobError::new("no_clip", false, "synthesize the sentence first"))?;
    let mut window = RenderWindow {
        src_start_ms: segment.start_ms.max(0),
        src_end_ms: segment.end_ms,
        dub_start_ms: 0,
    };
    if window.src_end_ms <= window.src_start_ms {
        return Err(invalid("sentence has no duration"));
    }
    if mode == SpatialMode::Concurrent {
        // Same placement as the default export (`MixOptions.placement` default).
        let rows =
            placement::load_dub_sentences(db.connection(), segment.media_id).map_err(db_err)?;
        let sentences = rows.iter().map(|r| r.sentence.clone()).collect::<Vec<_>>();
        let plan = placement::place(&sentences, &PlacementParams::default()).map_err(invalid)?;
        let placed = plan
            .iter()
            .find(|p| p.segment_id == segment_id && p.is_placed())
            .ok_or_else(|| JobError::new("not_placed", false, "sentence is not placed"))?;
        window.dub_start_ms = placed.dub_start_ms.max(0);
    }
    Ok(RenderPlan {
        media_id: segment.media_id,
        reference: reference_path(db, segment.media_id)?,
        tts_key: tts.key,
        clip: tts.path,
        window,
    })
}

/// Spatializes one Segment's current TTS clip (analysis first if needed).
pub fn spatialize(
    db: &Mutex<MediaDb>,
    store: &AssetStore,
    spec: &WorkerSpec,
    job: &JobHandle,
    segment_id: i64,
    mode: SpatialMode,
) -> Result<RenderOutcome, JobError> {
    let plan = plan_render(&*lock(db)?, store, segment_id, mode)?;
    let analysis = ensure_analysis(db, store, spec, job, plan.media_id)?;
    let key = render_key(
        &plan.tts_key,
        &analysis.spatial_track_key,
        mode,
        &plan.window,
        &Strengths::default(),
        RENDERER_VERSION,
    )
    .map_err(internal)?;
    let outcome = |path: &Path, cache_hit: bool, report: Option<Value>| RenderOutcome {
        segment_id,
        media_id: plan.media_id,
        mode,
        render_key: key.clone(),
        spatial_track_key: analysis.spatial_track_key.clone(),
        tts_asset_key: plan.tts_key.clone(),
        path: path.to_string_lossy().into_owned(),
        cache_hit,
        analysis_cache_hit: analysis.cache_hit,
        report,
    };
    if let Some((_, path)) =
        verified_asset(lock(db)?.connection(), store, &key, KIND_RENDER).map_err(db_err)?
    {
        return Ok(outcome(&path, true, None));
    }
    if job.is_cancelled() {
        return Err(JobError::cancelled());
    }
    let scratch = scratch_dir(store, job, "render");
    let track = PathBuf::from(&analysis.track_path);
    let mut window = json!({
        "src_start_ms": plan.window.src_start_ms,
        "src_end_ms": plan.window.src_end_ms,
    });
    if mode == SpatialMode::Concurrent {
        window["dub_start_ms"] = json!(plan.window.dub_start_ms);
    }
    let input = json!({
        "op": "render",
        "clip_path": plan.clip.to_string_lossy(),
        "reference_path": plan.reference.to_string_lossy(),
        "spatial_track_path": track.to_string_lossy(),
        "spatial_track_key": analysis.spatial_track_key,
        "reference_sha256": analysis.reference_sha256,
        "tts_asset_key": plan.tts_key,
        "mode": mode.as_str(),
        "window": window,
        "strengths": Strengths::default().to_value(),
        "output_name": format!("{key}.wav"),
    });
    let result = job.run_worker(
        spec,
        CAPABILITY,
        input,
        vec![plan.clip.clone(), plan.reference.clone(), track],
        scratch.0.clone(),
    )?;
    let output = &result.output;
    if output.get("render_key").and_then(Value::as_str) != Some(key.as_str()) {
        return Err(JobError::new(
            "spatial_key_mismatch",
            false,
            "the spatial worker's render key differs from the host's",
        ));
    }
    let relative = render_relative_path(&key).map_err(internal)?;
    let dest = store.absolute(&relative).map_err(io_err)?;
    worker::promote_output(Path::new(output_str(output, "path")?), &dest).map_err(io_err)?;
    let asset = SpatialAsset {
        key: key.clone(),
        kind: KIND_RENDER.to_string(),
        media_id: plan.media_id,
        segment_id: Some(segment_id),
        path: relative,
        sha256: worker::sha256_file(&dest).map_err(io_err)?,
        created_at: String::new(),
    };
    upsert_asset(lock(db)?.connection(), &asset).map_err(db_err)?;
    Ok(outcome(&dest, false, output.get("report").cloned()))
}

// ----- jobs + Tauri surface -----

/// `input_json` of a `spatial` job.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
enum SpatialJobInput {
    Analyze {
        media_id: i64,
    },
    Render {
        segment_id: i64,
        #[serde(default)]
        mode: SpatialMode,
    },
}

/// Runner for `spatial` jobs (registered at startup; also used by the commands).
pub fn spatial_runner(app: tauri::AppHandle) -> JobRunner {
    std::sync::Arc::new(move |job: &JobHandle| {
        use tauri::Manager;
        let input: SpatialJobInput = serde_json::from_value(job.input())
            .map_err(|e| invalid(format!("invalid spatial job input: {e}")))?;
        let state = app.state::<crate::AppState>();
        let store = tts::asset_store(&app).map_err(io_err)?;
        let spec = worker_spec(&WorkerRoots::from_app(&app))
            .map_err(|reason| JobError::new("runtime_missing", false, reason))?;
        let value = match input {
            SpatialJobInput::Analyze { media_id } => {
                serde_json::to_value(ensure_analysis(&state.db, &store, &spec, job, media_id)?)
            }
            SpatialJobInput::Render { segment_id, mode } => {
                serde_json::to_value(spatialize(&state.db, &store, &spec, job, segment_id, mode)?)
            }
        };
        value.map_err(internal)
    })
}

/// Analyses a media file's spatial cues (cached per media) as a `spatial` job.
/// Returns the `AnalysisOutcome` JSON.
#[tauri::command]
pub async fn analyze_spatial(app: tauri::AppHandle, media_id: i64) -> Result<Value, String> {
    use tauri::Manager;
    let input =
        serde_json::to_value(SpatialJobInput::Analyze { media_id }).map_err(|e| e.to_string())?;
    let store = std::sync::Arc::clone(&app.state::<crate::jobs::JobsState>().0);
    crate::jobs::run_to_completion(
        store,
        NewJob::new(SPATIAL_JOB, media_id).input(input),
        spatial_runner(app.clone()),
    )
    .await
}

/// Spatializes one Segment's TTS clip as a `spatial` job (analysis first when not
/// cached). `mode` defaults to `source_trajectory`. Returns the `RenderOutcome` JSON.
#[tauri::command]
pub async fn spatialize_segment(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
    segment_id: i64,
    mode: Option<SpatialMode>,
) -> Result<Value, String> {
    use tauri::Manager;
    let media_id = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        db.get_segment(segment_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "segment not found".to_string())?
            .media_id
    };
    let input = serde_json::to_value(SpatialJobInput::Render {
        segment_id,
        mode: mode.unwrap_or_default(),
    })
    .map_err(|e| e.to_string())?;
    let store = std::sync::Arc::clone(&app.state::<crate::jobs::JobsState>().0);
    crate::jobs::run_to_completion(
        store,
        NewJob::new(SPATIAL_JOB, media_id)
            .segment(segment_id)
            .input(input),
        spatial_runner(app.clone()),
    )
    .await
}

#[cfg(test)]
mod tests;
