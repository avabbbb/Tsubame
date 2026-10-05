use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;
use std::path::Path;

#[derive(Debug, Clone, Serialize)]
pub struct MediaItem {
    pub id: i64,
    pub path: String,
    pub title: String,
    pub media_type: String,
    pub duration_ms: i64,
    pub playback_position: i64,
    pub file_size: i64,
    pub speed: f64,
    pub volume: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct Segment {
    pub id: i64,
    pub media_id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub source_text: String,
    pub translated_text: String,
    pub ordinal: i64,
    pub revision: i64,
    pub asr_provenance: Option<String>,
    pub refine_provenance: Option<String>,
    pub translation_provenance: Option<String>,
    pub tts_provenance: Option<String>,
    pub translation_dirty: bool,
    pub tts_dirty: bool,
    pub mix_dirty: bool,
    pub subtitle_dirty: bool,
}

#[derive(Debug, Clone)]
pub struct SegmentDraft {
    pub start_ms: i64,
    pub end_ms: i64,
    pub source_text: String,
}

pub struct MediaDb {
    conn: Connection,
}

impl MediaDb {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(
            "PRAGMA foreign_keys = ON;
             CREATE TABLE IF NOT EXISTS media_files (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               path TEXT NOT NULL UNIQUE,
               title TEXT NOT NULL,
               media_type TEXT NOT NULL,
               duration_ms INTEGER NOT NULL DEFAULT 0,
               playback_position INTEGER NOT NULL DEFAULT 0,
               file_size INTEGER NOT NULL DEFAULT 0,
               speed REAL NOT NULL DEFAULT 1.0,
               volume REAL NOT NULL DEFAULT 1.0,
               added_at TEXT NOT NULL DEFAULT (datetime('now'))
             );
             CREATE TABLE IF NOT EXISTS segments (
               id INTEGER PRIMARY KEY AUTOINCREMENT,
               media_id INTEGER NOT NULL,
               start_ms INTEGER NOT NULL,
               end_ms INTEGER NOT NULL,
               source_text TEXT NOT NULL DEFAULT '',
               translated_text TEXT NOT NULL DEFAULT '',
               ordinal INTEGER NOT NULL DEFAULT 0,
               revision INTEGER NOT NULL DEFAULT 0,
               asr_provenance TEXT,
               refine_provenance TEXT,
               translation_provenance TEXT,
               tts_provenance TEXT,
               translation_dirty INTEGER NOT NULL DEFAULT 0,
               tts_dirty INTEGER NOT NULL DEFAULT 0,
               mix_dirty INTEGER NOT NULL DEFAULT 0,
               subtitle_dirty INTEGER NOT NULL DEFAULT 0,
               FOREIGN KEY (media_id) REFERENCES media_files(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_segments_media_time
               ON segments(media_id, start_ms, ordinal);",
        )?;

        Self::ensure_media_column(&conn, "speed", "REAL NOT NULL DEFAULT 1.0")?;
        Self::ensure_media_column(&conn, "volume", "REAL NOT NULL DEFAULT 1.0")?;
        Self::ensure_segment_column(&conn, "asr_provenance", "TEXT")?;
        Self::ensure_segment_column(&conn, "refine_provenance", "TEXT")?;
        Self::ensure_segment_column(&conn, "translation_provenance", "TEXT")?;
        Self::ensure_segment_column(&conn, "tts_provenance", "TEXT")?;
        Self::ensure_segment_column(
            &conn,
            "translation_dirty",
            "INTEGER NOT NULL DEFAULT 0",
        )?;
        Self::ensure_segment_column(&conn, "tts_dirty", "INTEGER NOT NULL DEFAULT 0")?;
        Self::ensure_segment_column(&conn, "mix_dirty", "INTEGER NOT NULL DEFAULT 0")?;
        Self::ensure_segment_column(
            &conn,
            "subtitle_dirty",
            "INTEGER NOT NULL DEFAULT 0",
        )?;

        Ok(Self { conn })
    }

    fn ensure_column(
        conn: &Connection,
        table: &str,
        column: &str,
        definition: &str,
    ) -> rusqlite::Result<()> {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        if !columns.iter().any(|name| name == column) {
            conn.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN {column} {definition};"
            ))?;
        }
        Ok(())
    }

    fn ensure_media_column(
        conn: &Connection,
        column: &str,
        definition: &str,
    ) -> rusqlite::Result<()> {
        Self::ensure_column(conn, "media_files", column, definition)
    }

    fn ensure_segment_column(
        conn: &Connection,
        column: &str,
        definition: &str,
    ) -> rusqlite::Result<()> {
        Self::ensure_column(conn, "segments", column, definition)
    }

    pub fn import_media(&self, path: &str) -> rusqlite::Result<MediaItem> {
        let p = Path::new(path);
        let title = p.file_stem().and_then(|s| s.to_str()).unwrap_or("Untitled");
        let ext = p
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let media_type = if matches!(ext.as_str(), "mp4" | "m4v" | "webm" | "mkv" | "mov" | "avi") {
            "video"
        } else {
            "audio"
        };
        let file_size = std::fs::metadata(p).map(|m| m.len() as i64).unwrap_or(0);
        self.conn.execute(
            "INSERT INTO media_files(path,title,media_type,file_size) VALUES(?1,?2,?3,?4)
             ON CONFLICT(path) DO UPDATE SET title=excluded.title, media_type=excluded.media_type, file_size=excluded.file_size",
            params![path, title, media_type, file_size],
        )?;
        self.media_by_path(path)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)
    }

    fn media_by_path(&self, path: &str) -> rusqlite::Result<Option<MediaItem>> {
        self.conn
            .query_row(
                "SELECT id,path,title,media_type,duration_ms,playback_position,file_size,
                        COALESCE(speed,1.0),COALESCE(volume,1.0)
                 FROM media_files WHERE path=?1",
                [path],
                |r| {
                    Ok(MediaItem {
                        id: r.get(0)?,
                        path: r.get(1)?,
                        title: r.get(2)?,
                        media_type: r.get(3)?,
                        duration_ms: r.get(4)?,
                        playback_position: r.get(5)?,
                        file_size: r.get(6)?,
                        speed: r.get(7)?,
                        volume: r.get(8)?,
                    })
                },
            )
            .optional()
    }

    pub fn list_media(&self) -> rusqlite::Result<Vec<MediaItem>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,path,title,media_type,duration_ms,playback_position,file_size,
                    COALESCE(speed,1.0),COALESCE(volume,1.0)
             FROM media_files ORDER BY added_at DESC,id DESC",
        )?;
        stmt.query_map([], |r| {
            Ok(MediaItem {
                id: r.get(0)?,
                path: r.get(1)?,
                title: r.get(2)?,
                media_type: r.get(3)?,
                duration_ms: r.get(4)?,
                playback_position: r.get(5)?,
                file_size: r.get(6)?,
                speed: r.get(7)?,
                volume: r.get(8)?,
            })
        })?
        .collect()
    }

    pub fn update_media_duration(&self, id: i64, duration_ms: i64) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE media_files SET duration_ms=?1 WHERE id=?2",
            params![duration_ms, id],
        )?;
        Ok(())
    }

    pub fn save_playback_state(
        &self,
        id: i64,
        position_ms: i64,
        speed: f64,
        volume: f64,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "UPDATE media_files
             SET playback_position=?1,speed=?2,volume=?3
             WHERE id=?4",
            params![
                position_ms.max(0),
                speed.clamp(0.5, 3.0),
                volume.clamp(0.0, 1.0),
                id
            ],
        )?;
        Ok(())
    }

    fn row_to_segment(row: &rusqlite::Row<'_>) -> rusqlite::Result<Segment> {
        Ok(Segment {
            id: row.get(0)?,
            media_id: row.get(1)?,
            start_ms: row.get(2)?,
            end_ms: row.get(3)?,
            source_text: row.get(4)?,
            translated_text: row.get(5)?,
            ordinal: row.get(6)?,
            revision: row.get(7)?,
            asr_provenance: row.get(8)?,
            refine_provenance: row.get(9)?,
            translation_provenance: row.get(10)?,
            tts_provenance: row.get(11)?,
            translation_dirty: row.get::<_, i64>(12)? != 0,
            tts_dirty: row.get::<_, i64>(13)? != 0,
            mix_dirty: row.get::<_, i64>(14)? != 0,
            subtitle_dirty: row.get::<_, i64>(15)? != 0,
        })
    }

    const SEGMENT_COLUMNS: &'static str =
        "id,media_id,start_ms,end_ms,source_text,translated_text,ordinal,revision,
         asr_provenance,refine_provenance,translation_provenance,tts_provenance,
         translation_dirty,tts_dirty,mix_dirty,subtitle_dirty";

    pub fn get_segments(&self, media_id: i64) -> rusqlite::Result<Vec<Segment>> {
        let sql = format!(
            "SELECT {} FROM segments WHERE media_id=?1 ORDER BY start_ms,ordinal,id",
            Self::SEGMENT_COLUMNS
        );
        let mut stmt = self.conn.prepare(&sql)?;
        stmt.query_map([media_id], Self::row_to_segment)?.collect()
    }

    pub fn get_segment(&self, id: i64) -> rusqlite::Result<Option<Segment>> {
        let sql = format!("SELECT {} FROM segments WHERE id=?1", Self::SEGMENT_COLUMNS);
        self.conn
            .query_row(&sql, [id], Self::row_to_segment)
            .optional()
    }

    pub fn update_segment(
        &self,
        id: i64,
        expected_revision: i64,
        source_text: &str,
        translated_text: &str,
        start_ms: i64,
        end_ms: i64,
    ) -> rusqlite::Result<Option<Segment>> {
        if start_ms < 0 || end_ms <= start_ms {
            return Ok(None);
        }

        let Some(previous) = self.get_segment(id)? else {
            return Ok(None);
        };
        if previous.revision != expected_revision {
            return Ok(None);
        }

        let source_changed = previous.source_text != source_text;
        let translation_changed = previous.translated_text != translated_text;
        let timing_changed = previous.start_ms != start_ms || previous.end_ms != end_ms;

        let translation_dirty = previous.translation_dirty || source_changed;
        let tts_dirty = previous.tts_dirty || source_changed || translation_changed;
        let mix_dirty =
            previous.mix_dirty || source_changed || translation_changed || timing_changed;
        let subtitle_dirty =
            previous.subtitle_dirty || source_changed || translation_changed || timing_changed;

        let changed = self.conn.execute(
            "UPDATE segments
             SET source_text=?1,translated_text=?2,start_ms=?3,end_ms=?4,
                 revision=revision+1,
                 translation_dirty=?5,tts_dirty=?6,mix_dirty=?7,subtitle_dirty=?8,
                 asr_provenance=CASE WHEN ?9 THEN 'manual' ELSE asr_provenance END,
                 translation_provenance=CASE WHEN ?10 THEN 'manual' ELSE translation_provenance END
             WHERE id=?11 AND revision=?12",
            params![
                source_text,
                translated_text,
                start_ms,
                end_ms,
                translation_dirty as i64,
                tts_dirty as i64,
                mix_dirty as i64,
                subtitle_dirty as i64,
                source_changed,
                translation_changed,
                id,
                expected_revision
            ],
        )?;

        if changed == 0 {
            return Ok(None);
        }
        self.get_segment(id)
    }

    pub fn replace_source_segments(
        &self,
        media_id: i64,
        drafts: &[SegmentDraft],
        provenance: &str,
    ) -> rusqlite::Result<Vec<Segment>> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM segments WHERE media_id=?1", [media_id])?;
        for (ordinal, draft) in drafts.iter().enumerate() {
            tx.execute(
                "INSERT INTO segments(
                    media_id,start_ms,end_ms,source_text,ordinal,revision,
                    asr_provenance,translation_dirty,tts_dirty,mix_dirty,subtitle_dirty
                 ) VALUES(?1,?2,?3,?4,?5,0,?6,1,1,1,1)",
                params![
                    media_id,
                    draft.start_ms,
                    draft.end_ms,
                    draft.source_text,
                    ordinal as i64,
                    provenance
                ],
            )?;
        }
        tx.commit()?;
        self.get_segments(media_id)
    }

    pub fn import_translation_segments(
        &self,
        media_id: i64,
        drafts: &[SegmentDraft],
        provenance: &str,
    ) -> rusqlite::Result<Vec<Segment>> {
        let existing = self.get_segments(media_id)?;
        if existing.len() != drafts.len() {
            return Err(rusqlite::Error::InvalidParameterName(format!(
                "translation cue count {} does not match segment count {}",
                drafts.len(),
                existing.len()
            )));
        }

        let tx = self.conn.unchecked_transaction()?;
        for (segment, draft) in existing.iter().zip(drafts.iter()) {
            tx.execute(
                "UPDATE segments
                 SET translated_text=?1,translation_provenance=?2,
                     revision=revision+1,tts_dirty=1,mix_dirty=1,subtitle_dirty=1
                 WHERE id=?3",
                params![draft.source_text, provenance, segment.id],
            )?;
        }
        tx.commit()?;
        self.get_segments(media_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn playback_state_roundtrip() -> rusqlite::Result<()> {
        let db = MediaDb::open(Path::new(":memory:"))?;
        let media = db.import_media("example.mp3")?;
        db.save_playback_state(media.id, 42_000, 1.25, 0.65)?;
        let row = db.list_media()?.remove(0);
        assert_eq!(row.playback_position, 42_000);
        assert!((row.speed - 1.25).abs() < f64::EPSILON);
        assert!((row.volume - 0.65).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn dirty_dependencies_are_selective() -> rusqlite::Result<()> {
        let db = MediaDb::open(Path::new(":memory:"))?;
        let media = db.import_media("example.mp3")?;
        let segments = db.replace_source_segments(
            media.id,
            &[SegmentDraft {
                start_ms: 1000,
                end_ms: 2000,
                source_text: "hello".into(),
            }],
            "subtitle:test",
        )?;
        let original = &segments[0];

        let translated = db
            .update_segment(
                original.id,
                original.revision,
                &original.source_text,
                "こんにちは",
                original.start_ms,
                original.end_ms,
            )?
            .unwrap();

        assert!(!translated.translation_dirty);
        assert!(translated.tts_dirty);
        assert!(translated.mix_dirty);
        assert!(translated.subtitle_dirty);
        Ok(())
    }
}
