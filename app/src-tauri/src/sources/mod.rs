//! Source Registry (PR #14, `docs/08-source-adapters.md`).
//!
//! Content acquisition is a pluggable capability: every source implements
//! [SourceAdapter] and normalizes remote or local metadata into [Work] and
//! [Asset]. Adapters own only the `source_works` / `source_assets` tables and
//! the `downloads` queue; a failing adapter never writes `media_files` or
//! `segments` (importing into the library is a separate, explicit step).

pub mod dlsite;
pub mod dlsite_auth;
pub mod download;
pub mod local;

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

/// What kind of file an asset is; the UI asks "what does this work contain?".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AssetKind {
    Audio,
    Video,
    Subtitle,
    Image,
    Archive,
    Text,
    Metadata,
}

impl AssetKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Audio => "audio",
            Self::Video => "video",
            Self::Subtitle => "subtitle",
            Self::Image => "image",
            Self::Archive => "archive",
            Self::Text => "text",
            Self::Metadata => "metadata",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "audio" => Self::Audio,
            "video" => Self::Video,
            "subtitle" => Self::Subtitle,
            "image" => Self::Image,
            "archive" => Self::Archive,
            "text" => Self::Text,
            "metadata" => Self::Metadata,
            _ => return None,
        })
    }
}

/// One file a work offers: a local path or a remote URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Asset {
    /// Stable within its source (local: absolute path; remote: adapter id).
    pub id: String,
    pub kind: AssetKind,
    pub name: String,
    /// Absolute path for local assets, URL for remote ones.
    pub location: String,
    pub size: Option<u64>,
    pub sha256: Option<String>,
}

/// A voice actor: the original Japanese name is canonical, romanized or
/// alternative spellings are aliases.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VoiceActor {
    pub name_ja: String,
    pub aliases: Vec<String>,
}

/// A work normalized from any source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Work {
    pub source_id: String,
    pub remote_id: String,
    pub rj_code: Option<String>,
    pub title: String,
    pub circle: Option<String>,
    pub voice_actors: Vec<VoiceActor>,
    pub languages: Vec<String>,
    pub tags: Vec<String>,
    pub cover: Option<String>,
    /// ISO date (YYYY-MM-DD).
    pub release_date: Option<String>,
    pub assets: Vec<Asset>,
}

/// What an adapter implements; everything else fails with `unsupported`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub search: bool,
    pub get_work: bool,
    pub list_assets: bool,
    pub resolve_asset: bool,
    pub acquire: bool,
    pub refresh: bool,
    /// Calls the network (only on an explicit operation, never in the background).
    pub network: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SearchFilters {
    #[serde(default)]
    pub limit: Option<u32>,
}

/// Typed adapter failure. `code` is the machine contract
/// (unsupported, invalid_input, not_found, network, parse, io, db).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceError {
    pub code: String,
    pub retryable: bool,
    pub message: String,
}

impl SourceError {
    pub fn new(code: impl Into<String>, retryable: bool, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            retryable,
            message: message.into(),
        }
    }

    pub fn unsupported(source_id: &str, operation: &str) -> Self {
        Self::new(
            "unsupported",
            false,
            format!("source {source_id} does not support {operation}"),
        )
    }

    pub fn invalid_input(message: impl Into<String>) -> Self {
        Self::new("invalid_input", false, message)
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        Self::new("not_found", false, message)
    }

    pub fn network(message: impl Into<String>) -> Self {
        Self::new("network", true, message)
    }

    pub fn parse(message: impl Into<String>) -> Self {
        Self::new("parse", false, message)
    }
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl From<rusqlite::Error> for SourceError {
    fn from(error: rusqlite::Error) -> Self {
        Self::new("db", false, error.to_string())
    }
}

impl From<std::io::Error> for SourceError {
    fn from(error: std::io::Error) -> Self {
        Self::new("io", false, error.to_string())
    }
}

/// The adapter contract from docs/08. Methods an adapter does not support
/// keep the default, which fails with a typed `unsupported` error.
// list_assets/resolve_asset/acquire/refresh are the docs/08 contract; their
// callers (DLsite library, library browser) are follow-ups.
#[allow(dead_code)]
pub trait SourceAdapter: Send + Sync {
    fn id(&self) -> &'static str;
    fn label(&self) -> &'static str;
    fn capabilities(&self) -> Capabilities;

    fn search(&self, _query: &str, _filters: &SearchFilters) -> Result<Vec<Work>, SourceError> {
        Err(SourceError::unsupported(self.id(), "search"))
    }

    fn get_work(&self, _remote_id: &str) -> Result<Work, SourceError> {
        Err(SourceError::unsupported(self.id(), "get_work"))
    }

    fn list_assets(&self, _remote_id: &str) -> Result<Vec<Asset>, SourceError> {
        Err(SourceError::unsupported(self.id(), "list_assets"))
    }

    fn resolve_asset(&self, _asset_id: &str) -> Result<Asset, SourceError> {
        Err(SourceError::unsupported(self.id(), "resolve_asset"))
    }

    fn acquire(
        &self,
        _asset_id: &str,
        _destination: &std::path::Path,
    ) -> Result<std::path::PathBuf, SourceError> {
        Err(SourceError::unsupported(self.id(), "acquire"))
    }

    fn refresh(&self, _record: &Work) -> Result<Work, SourceError> {
        Err(SourceError::unsupported(self.id(), "refresh"))
    }
}

/// `source.list` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SourceInfo {
    pub id: &'static str,
    pub label: &'static str,
    pub capabilities: Capabilities,
}

/// Registered adapters. Only implemented, lawful sources are listed.
pub fn adapters() -> Vec<Box<dyn SourceAdapter>> {
    vec![
        Box::new(local::LocalAdapter),
        Box::new(dlsite::DlsiteAdapter::new()),
    ]
}

pub fn list() -> Vec<SourceInfo> {
    adapters()
        .iter()
        .map(|adapter| SourceInfo {
            id: adapter.id(),
            label: adapter.label(),
            capabilities: adapter.capabilities(),
        })
        .collect()
}

/// The registered adapter with this id.
pub fn adapter(source_id: &str) -> Option<Box<dyn SourceAdapter>> {
    adapters()
        .into_iter()
        .find(|adapter| adapter.id() == source_id)
}

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS source_works (
           source_id TEXT NOT NULL,
           remote_id TEXT NOT NULL,
           rj_code TEXT,
           title TEXT NOT NULL,
           circle TEXT,
           voice_actors_json TEXT NOT NULL DEFAULT '[]',
           languages_json TEXT NOT NULL DEFAULT '[]',
           tags_json TEXT NOT NULL DEFAULT '[]',
           cover TEXT,
           release_date TEXT,
           fetched_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
           PRIMARY KEY (source_id, remote_id)
         );
         CREATE INDEX IF NOT EXISTS idx_source_works_rj ON source_works(rj_code);
         CREATE TABLE IF NOT EXISTS source_assets (
           source_id TEXT NOT NULL,
           remote_id TEXT NOT NULL,
           asset_id TEXT NOT NULL,
           kind TEXT NOT NULL
             CHECK (kind IN ('audio','video','subtitle','image','archive','text','metadata')),
           name TEXT NOT NULL,
           location TEXT NOT NULL,
           size INTEGER,
           sha256 TEXT,
           ordinal INTEGER NOT NULL DEFAULT 0,
           PRIMARY KEY (source_id, remote_id, asset_id),
           FOREIGN KEY (source_id, remote_id)
             REFERENCES source_works(source_id, remote_id) ON DELETE CASCADE
         );
         CREATE TABLE IF NOT EXISTS downloads (
           id TEXT PRIMARY KEY,
           url TEXT NOT NULL,
           dest TEXT NOT NULL,
           staging_path TEXT NOT NULL,
           bytes_done INTEGER NOT NULL DEFAULT 0,
           total INTEGER,
           sha256 TEXT,
           status TEXT NOT NULL DEFAULT 'queued'
             CHECK (status IN ('queued','running','succeeded','failed','cancelled')),
           attempts INTEGER NOT NULL DEFAULT 0,
           error TEXT,
           job_id TEXT,
           created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
           updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
           UNIQUE (url, dest)
         );
         CREATE INDEX IF NOT EXISTS idx_downloads_status ON downloads(status, created_at);",
    )?;
    // download.enqueue provenance, added after the table shipped: guarded.
    for (column, decl) in [
        ("source_id", "TEXT"),
        ("remote_id", "TEXT"),
        ("asset_id", "TEXT"),
        ("import", "INTEGER NOT NULL DEFAULT 0"),
    ] {
        if !has_column(conn, "downloads", column)? {
            conn.execute_batch(&format!("ALTER TABLE downloads ADD COLUMN {column} {decl}"))?;
        }
    }
    // Search hits carry only list fields; source.get_work refetches these.
    if !has_column(conn, "source_works", "partial")? {
        conn.execute_batch(
            "ALTER TABLE source_works ADD COLUMN partial INTEGER NOT NULL DEFAULT 0",
        )?;
    }
    Ok(())
}

fn has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let names = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(names.iter().any(|name| name == column))
}

fn to_json<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "[]".into())
}

/// Stores (or replaces) a work and its assets in one transaction. Touches
/// only the source_* tables.
pub fn upsert_work(conn: &Connection, work: &Work) -> Result<(), SourceError> {
    let tx = conn.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO source_works
           (source_id, remote_id, rj_code, title, circle, voice_actors_json,
            languages_json, tags_json, cover, release_date)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
         ON CONFLICT (source_id, remote_id) DO UPDATE SET
           rj_code = excluded.rj_code, title = excluded.title, circle = excluded.circle,
           voice_actors_json = excluded.voice_actors_json,
           languages_json = excluded.languages_json, tags_json = excluded.tags_json,
           cover = excluded.cover, release_date = excluded.release_date,
           partial = 0,
           fetched_at = strftime('%Y-%m-%dT%H:%M:%fZ','now')",
        params![
            work.source_id,
            work.remote_id,
            work.rj_code,
            work.title,
            work.circle,
            to_json(&work.voice_actors),
            to_json(&work.languages),
            to_json(&work.tags),
            work.cover,
            work.release_date,
        ],
    )?;
    tx.execute(
        "DELETE FROM source_assets WHERE source_id = ?1 AND remote_id = ?2",
        params![work.source_id, work.remote_id],
    )?;
    insert_assets(&tx, work)?;
    tx.commit()?;
    Ok(())
}

fn insert_assets(conn: &Connection, work: &Work) -> rusqlite::Result<()> {
    for (ordinal, asset) in work.assets.iter().enumerate() {
        conn.execute(
            "INSERT INTO source_assets
               (source_id, remote_id, asset_id, kind, name, location, size, sha256, ordinal)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                work.source_id,
                work.remote_id,
                asset.id,
                asset.kind.as_str(),
                asset.name,
                asset.location,
                asset.size.map(|size| size as i64),
                asset.sha256,
                ordinal as i64,
            ],
        )?;
    }
    Ok(())
}

/// Caches a search hit only when nothing is cached yet, marked partial so
/// source.get_work fetches the full work instead of returning it.
pub fn cache_search_hit(conn: &Connection, work: &Work) -> Result<(), SourceError> {
    let tx = conn.unchecked_transaction()?;
    let inserted = tx.execute(
        "INSERT INTO source_works
           (source_id, remote_id, rj_code, title, circle, voice_actors_json,
            languages_json, tags_json, cover, release_date, partial)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, 1)
         ON CONFLICT (source_id, remote_id) DO NOTHING",
        params![
            work.source_id,
            work.remote_id,
            work.rj_code,
            work.title,
            work.circle,
            to_json(&work.voice_actors),
            to_json(&work.languages),
            to_json(&work.tags),
            work.cover,
            work.release_date,
        ],
    )?;
    if inserted > 0 {
        insert_assets(&tx, work)?;
    }
    tx.commit()?;
    Ok(())
}

/// Whether the cached work is only a search hit (false when not cached).
pub fn is_partial(
    conn: &Connection,
    source_id: &str,
    remote_id: &str,
) -> Result<bool, SourceError> {
    let partial: Option<i64> = conn
        .query_row(
            "SELECT partial FROM source_works WHERE source_id = ?1 AND remote_id = ?2",
            params![source_id, remote_id],
            |row| row.get(0),
        )
        .optional()?;
    Ok(partial.is_some_and(|flag| flag != 0))
}

pub fn get_work(
    conn: &Connection,
    source_id: &str,
    remote_id: &str,
) -> Result<Option<Work>, SourceError> {
    let row = conn
        .query_row(
            "SELECT rj_code, title, circle, voice_actors_json, languages_json, tags_json,
                    cover, release_date
             FROM source_works WHERE source_id = ?1 AND remote_id = ?2",
            params![source_id, remote_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, Option<String>>(6)?,
                    row.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()?;
    let Some((rj_code, title, circle, actors, languages, tags, cover, release_date)) = row else {
        return Ok(None);
    };
    let mut stmt = conn.prepare(
        "SELECT asset_id, kind, name, location, size, sha256 FROM source_assets
         WHERE source_id = ?1 AND remote_id = ?2 ORDER BY ordinal",
    )?;
    let assets = stmt
        .query_map(params![source_id, remote_id], |row| {
            let kind: String = row.get(1)?;
            Ok(Asset {
                id: row.get(0)?,
                kind: AssetKind::parse(&kind).unwrap_or(AssetKind::Metadata),
                name: row.get(2)?,
                location: row.get(3)?,
                size: row.get::<_, Option<i64>>(4)?.map(|size| size.max(0) as u64),
                sha256: row.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(Some(Work {
        source_id: source_id.to_string(),
        remote_id: remote_id.to_string(),
        rj_code,
        title,
        circle,
        voice_actors: serde_json::from_str(&actors).unwrap_or_default(),
        languages: serde_json::from_str(&languages).unwrap_or_default(),
        tags: serde_json::from_str(&tags).unwrap_or_default(),
        cover,
        release_date,
        assets,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn work() -> Work {
        Work {
            source_id: "dlsite".into(),
            remote_id: "RJ000001".into(),
            rj_code: Some("RJ000001".into()),
            title: "おやすみ".into(),
            circle: Some("サークル".into()),
            voice_actors: vec![VoiceActor {
                name_ja: "山田花子".into(),
                aliases: vec!["Yamada Hanako".into()],
            }],
            languages: vec!["ja".into()],
            tags: vec!["癒し".into()],
            cover: Some("https://img.example/cover.jpg".into()),
            release_date: Some("2024-01-02".into()),
            assets: vec![Asset {
                id: "cover".into(),
                kind: AssetKind::Image,
                name: "cover.jpg".into(),
                location: "https://img.example/cover.jpg".into(),
                size: None,
                sha256: None,
            }],
        }
    }

    #[test]
    fn migrate_is_idempotent_and_works_roundtrip() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let mut stored = work();
        upsert_work(&conn, &stored).unwrap();
        stored.title = "おはよう".into();
        stored.assets.clear();
        upsert_work(&conn, &stored).unwrap();
        assert_eq!(get_work(&conn, "dlsite", "RJ000001").unwrap(), Some(stored));
        assert_eq!(get_work(&conn, "dlsite", "RJ999999").unwrap(), None);
    }

    #[test]
    fn search_hits_never_shadow_full_works() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("PRAGMA foreign_keys = ON;").unwrap();
        migrate(&conn).unwrap();
        let full = work();
        let mut hit = work();
        hit.voice_actors.clear();
        hit.tags.clear();
        hit.assets.clear();
        // An uncached hit is stored, marked partial.
        cache_search_hit(&conn, &hit).unwrap();
        assert!(is_partial(&conn, "dlsite", "RJ000001").unwrap());
        // A full fetch replaces it and clears the mark.
        upsert_work(&conn, &full).unwrap();
        assert!(!is_partial(&conn, "dlsite", "RJ000001").unwrap());
        // A later hit leaves the full work untouched.
        cache_search_hit(&conn, &hit).unwrap();
        assert!(!is_partial(&conn, "dlsite", "RJ000001").unwrap());
        assert_eq!(get_work(&conn, "dlsite", "RJ000001").unwrap(), Some(full));
        assert!(!is_partial(&conn, "dlsite", "RJ999999").unwrap());
    }

    #[test]
    fn unsupported_methods_fail_with_a_typed_error() {
        struct Bare;
        impl SourceAdapter for Bare {
            fn id(&self) -> &'static str {
                "bare"
            }
            fn label(&self) -> &'static str {
                "Bare"
            }
            fn capabilities(&self) -> Capabilities {
                Capabilities::default()
            }
        }
        let error = Bare.search("x", &SearchFilters::default()).unwrap_err();
        assert_eq!(error.code, "unsupported");
        assert!(!error.retryable);
        assert_eq!(Bare.refresh(&work()).unwrap_err().code, "unsupported");
    }

    #[test]
    fn registry_lists_only_implemented_sources() {
        let ids: Vec<&str> = list().iter().map(|info| info.id).collect();
        assert_eq!(ids, ["local", "dlsite"]);
    }
}
