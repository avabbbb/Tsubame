//! `audio.spatialize` host side: cache keys and the `spatial_assets` table for
//! the Python worker (`workers/dubber/spatial/worker.py`).
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

#[cfg(test)]
use crate::db::MediaDb;
use crate::tts::AssetStore;
use crate::worker;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::path::Path;
use std::path::PathBuf;

/// Must equal `spatial.ANALYZER_VERSION` in the worker.
pub const ANALYZER_VERSION: &str = "spatial-analyze/1";
/// Must equal `spatial.RENDERER_VERSION` in the worker.
pub const RENDERER_VERSION: &str = "spatial-render/1";

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

#[cfg(test)]
mod tests;
