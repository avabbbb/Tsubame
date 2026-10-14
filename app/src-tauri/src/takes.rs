//! Takes: the synthesized variants of one Segment (PR plan #11).
//!
//! Contract: `docs/20-segment-regenerate.md` § Takes.
//!
//! - A take is one `tts_takes` row: `(segment_id, take_index)` → `tts_asset_key`.
//!   `take_index` is the `take` field of the TTS cache key (`tts::cache_key`), so
//!   take 0 is exactly the PR #10 key.
//! - The active take is the one `segments.tts_asset_key` points at; placement,
//!   mix and spatial keep reading that column.
//! - At most [`MAX_TAKES_PER_SEGMENT`] takes are kept per Segment (never the
//!   active one). A pruned take's asset is deleted only when no take row and no
//!   Segment references its key any more (assets are shared across Segments by
//!   content addressing).

use crate::tts::{self, AssetStore, PreparedSynthesis};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

pub const MAX_TAKES_PER_SEGMENT: usize = 5;

/// Idempotent migration: `tts_takes`, `segments.dub_pushed_ms`, and a take-0
/// backfill for Segments that already point at a clip. Called from
/// `tts::migrate` (after `segments.tts_asset_key` exists).
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tts_takes (
           segment_id INTEGER NOT NULL,
           take_index INTEGER NOT NULL,
           tts_asset_key TEXT NOT NULL,
           text TEXT NOT NULL DEFAULT '',
           voice_profile_id TEXT NOT NULL DEFAULT '',
           created_at TEXT NOT NULL DEFAULT (datetime('now')),
           PRIMARY KEY (segment_id, take_index)
         );
         CREATE INDEX IF NOT EXISTS idx_tts_takes_key ON tts_takes(tts_asset_key);",
    )?;
    let has_pushed = {
        let mut stmt = conn.prepare("PRAGMA table_info(segments)")?;
        let names = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        names.iter().any(|name| name == "dub_pushed_ms")
    };
    if !has_pushed {
        conn.execute_batch(
            "ALTER TABLE segments ADD COLUMN dub_pushed_ms INTEGER NOT NULL DEFAULT 0;",
        )?;
    }
    // Backfill: a Segment with a clip but no take rows gets its clip as take 0.
    let legacy = {
        let mut stmt = conn.prepare(
            "SELECT s.id, s.tts_asset_key, s.translated_text, s.dirty_tts,
                    COALESCE(t.voice_profile_id, '')
               FROM segments s
               LEFT JOIN tts_assets t ON t.key = s.tts_asset_key
              WHERE s.tts_asset_key IS NOT NULL
                AND NOT EXISTS (SELECT 1 FROM tts_takes k WHERE k.segment_id = s.id)",
        )?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, bool>(3)?,
                    row.get::<_, String>(4)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        rows
    };
    for (segment_id, key, translated, dirty_tts, voice_profile_id) in legacy {
        // The text is only known when the clip is clean against the translation.
        let text = if dirty_tts {
            String::new()
        } else {
            tts::normalize_text(&translated)
        };
        conn.execute(
            "INSERT OR IGNORE INTO tts_takes(segment_id,take_index,tts_asset_key,text,voice_profile_id)
             VALUES(?1,0,?2,?3,?4)",
            params![segment_id, key, text, voice_profile_id],
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TakeRow {
    pub segment_id: i64,
    pub take_index: i64,
    pub tts_asset_key: String,
    /// Normalized text the take speaks; empty = unknown (legacy backfill).
    pub text: String,
    pub voice_profile_id: String,
    pub created_at: String,
}

/// A take as listed to the UI.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TakeInfo {
    pub segment_id: i64,
    pub take_index: i64,
    pub tts_asset_key: String,
    pub text: String,
    pub voice_profile_id: String,
    pub duration_ms: Option<i64>,
    /// Absolute clip path (frontend: `convertFileSrc`); `None` if the asset is gone.
    pub path: Option<String>,
    /// The asset row exists and its file is present.
    pub available: bool,
    /// `segments.tts_asset_key` points at this take.
    pub active: bool,
    pub created_at: String,
}

/// Rows removed by [`prune`] and the asset files that are no longer referenced.
#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct PruneResult {
    pub pruned_takes: Vec<i64>,
    pub deleted_assets: Vec<String>,
    /// Store-relative paths of `deleted_assets` (delete with [`delete_files`]).
    #[serde(skip)]
    pub deleted_paths: Vec<String>,
}

fn row_to_take(row: &rusqlite::Row<'_>) -> rusqlite::Result<TakeRow> {
    Ok(TakeRow {
        segment_id: row.get(0)?,
        take_index: row.get(1)?,
        tts_asset_key: row.get(2)?,
        text: row.get(3)?,
        voice_profile_id: row.get(4)?,
        created_at: row.get(5)?,
    })
}

/// A Segment's take rows, newest (highest index) first.
pub fn takes_for_segment(conn: &Connection, segment_id: i64) -> rusqlite::Result<Vec<TakeRow>> {
    let mut stmt = conn.prepare(
        "SELECT segment_id,take_index,tts_asset_key,text,voice_profile_id,created_at
           FROM tts_takes WHERE segment_id=?1 ORDER BY take_index DESC",
    )?;
    let rows = stmt
        .query_map([segment_id], row_to_take)?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(rows)
}

pub fn find_take(
    conn: &Connection,
    segment_id: i64,
    key: &str,
) -> rusqlite::Result<Option<TakeRow>> {
    conn.query_row(
        "SELECT segment_id,take_index,tts_asset_key,text,voice_profile_id,created_at
           FROM tts_takes WHERE segment_id=?1 AND tts_asset_key=?2
          ORDER BY take_index DESC LIMIT 1",
        params![segment_id, key],
        row_to_take,
    )
    .optional()
}

/// `max(take_index) + 1`, or 0 when the Segment has no takes.
pub fn next_take_index(conn: &Connection, segment_id: i64) -> rusqlite::Result<u32> {
    let next: i64 = conn.query_row(
        "SELECT COALESCE(MAX(take_index) + 1, 0) FROM tts_takes WHERE segment_id=?1",
        [segment_id],
        |row| row.get(0),
    )?;
    Ok(u32::try_from(next.max(0)).unwrap_or(u32::MAX))
}

/// Inserts (or replaces) the take row of a committed synthesis.
pub fn record_take(
    conn: &Connection,
    prepared: &PreparedSynthesis,
    take: u32,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO tts_takes(segment_id,take_index,tts_asset_key,text,voice_profile_id)
         VALUES(?1,?2,?3,?4,?5)
         ON CONFLICT(segment_id,take_index) DO UPDATE SET
           tts_asset_key=excluded.tts_asset_key,text=excluded.text,
           voice_profile_id=excluded.voice_profile_id,created_at=datetime('now')",
        params![
            prepared.segment.id,
            take,
            prepared.key,
            prepared.request.text,
            prepared.voice_profile_id
        ],
    )?;
    Ok(())
}

/// True when any take row or any Segment still points at `key`.
pub fn key_is_referenced(conn: &Connection, key: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM tts_takes WHERE tts_asset_key=?1)
             OR EXISTS(SELECT 1 FROM segments WHERE tts_asset_key=?1)",
        [key],
        |row| row.get::<_, bool>(0),
    )
}

/// Keeps the newest [`MAX_TAKES_PER_SEGMENT`] takes (plus the active take when
/// it is older) and deletes the `tts_assets` rows of keys nothing references
/// any more. Files are removed separately with [`delete_files`] (after the
/// surrounding transaction commits).
pub fn prune(conn: &Connection, segment_id: i64) -> rusqlite::Result<PruneResult> {
    let active: Option<String> = conn
        .query_row(
            "SELECT tts_asset_key FROM segments WHERE id=?1",
            [segment_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let rows = takes_for_segment(conn, segment_id)?;
    let mut result = PruneResult::default();
    let mut candidates: Vec<String> = Vec::new();
    for row in rows.iter().skip(MAX_TAKES_PER_SEGMENT) {
        if active.as_deref() == Some(row.tts_asset_key.as_str()) {
            continue;
        }
        conn.execute(
            "DELETE FROM tts_takes WHERE segment_id=?1 AND take_index=?2",
            params![segment_id, row.take_index],
        )?;
        result.pruned_takes.push(row.take_index);
        if !candidates.contains(&row.tts_asset_key) {
            candidates.push(row.tts_asset_key.clone());
        }
    }
    for key in candidates {
        if key_is_referenced(conn, &key)? {
            continue;
        }
        if let Some(asset) = tts::get_asset(conn, &key)? {
            conn.execute("DELETE FROM tts_assets WHERE key=?1", [&key])?;
            result.deleted_paths.push(asset.path);
            result.deleted_assets.push(key);
        }
    }
    Ok(result)
}

/// `record_take` + `prune` (the `synthesize_segment` path).
pub fn record_and_prune(
    conn: &Connection,
    prepared: &PreparedSynthesis,
    take: u32,
) -> rusqlite::Result<PruneResult> {
    record_take(conn, prepared, take)?;
    prune(conn, prepared.segment.id)
}

/// Removes pruned asset files. Missing files are ignored (best effort: the rows
/// are already gone, so nothing can resolve them).
pub fn delete_files(store: &AssetStore, relative_paths: &[String]) {
    for relative in relative_paths {
        if let Ok(path) = store.absolute(relative) {
            let _ = std::fs::remove_file(path);
        }
    }
}

/// The Segment's takes, newest first, with asset availability.
pub fn list_takes(
    conn: &Connection,
    store: &AssetStore,
    segment_id: i64,
) -> rusqlite::Result<Vec<TakeInfo>> {
    let active: Option<String> = conn
        .query_row(
            "SELECT tts_asset_key FROM segments WHERE id=?1",
            [segment_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let mut out = Vec::new();
    for row in takes_for_segment(conn, segment_id)? {
        let asset = tts::get_asset(conn, &row.tts_asset_key)?;
        let path = asset
            .as_ref()
            .and_then(|asset| store.absolute(&asset.path).ok())
            .filter(|path| path.is_file());
        out.push(TakeInfo {
            active: active.as_deref() == Some(row.tts_asset_key.as_str()),
            available: path.is_some(),
            duration_ms: asset.as_ref().map(|asset| asset.duration_ms),
            path: path.map(|p| p.to_string_lossy().into_owned()),
            segment_id: row.segment_id,
            take_index: row.take_index,
            tts_asset_key: row.tts_asset_key,
            text: row.text,
            voice_profile_id: row.voice_profile_id,
            created_at: row.created_at,
        });
    }
    Ok(out)
}

pub fn take_info(
    conn: &Connection,
    store: &AssetStore,
    segment_id: i64,
    key: &str,
) -> rusqlite::Result<Option<TakeInfo>> {
    Ok(list_takes(conn, store, segment_id)?
        .into_iter()
        .find(|take| take.tts_asset_key == key))
}
