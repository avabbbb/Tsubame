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
               FOREIGN KEY (media_id) REFERENCES media_files(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_segments_media_time
               ON segments(media_id, start_ms, ordinal);",
        )?;
        Self::ensure_media_column(&conn, "speed", "REAL NOT NULL DEFAULT 1.0")?;
        Self::ensure_media_column(&conn, "volume", "REAL NOT NULL DEFAULT 1.0")?;
        Ok(Self { conn })
    }

    fn ensure_media_column(
        conn: &Connection,
        column: &str,
        definition: &str,
    ) -> rusqlite::Result<()> {
        let mut stmt = conn.prepare("PRAGMA table_info(media_files)")?;
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))?
            .collect::<Result<Vec<_>, _>>()?;
        if !columns.iter().any(|name| name == column) {
            conn.execute_batch(&format!(
                "ALTER TABLE media_files ADD COLUMN {column} {definition};"
            ))?;
        }
        Ok(())
    }

    pub fn import_media(&self, path: &str) -> rusqlite::Result<MediaItem> {
        let p = Path::new(path);
        let title = p.file_stem().and_then(|s| s.to_str()).unwrap_or("Untitled");
        let ext = p
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let media_type = if matches!(
            ext.as_str(),
            "mp4" | "m4v" | "webm" | "mkv" | "mov" | "avi"
        ) {
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

    pub fn get_segments(&self, media_id: i64) -> rusqlite::Result<Vec<Segment>> {
        let mut stmt = self.conn.prepare(
            "SELECT id,media_id,start_ms,end_ms,source_text,translated_text,ordinal,revision
             FROM segments WHERE media_id=?1 ORDER BY start_ms,ordinal,id",
        )?;
        stmt.query_map([media_id], |r| {
            Ok(Segment {
                id: r.get(0)?,
                media_id: r.get(1)?,
                start_ms: r.get(2)?,
                end_ms: r.get(3)?,
                source_text: r.get(4)?,
                translated_text: r.get(5)?,
                ordinal: r.get(6)?,
                revision: r.get(7)?,
            })
        })?
        .collect()
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
        let changed = self.conn.execute(
            "UPDATE segments
             SET source_text=?1,translated_text=?2,start_ms=?3,end_ms=?4,revision=revision+1
             WHERE id=?5 AND revision=?6",
            params![
                source_text,
                translated_text,
                start_ms,
                end_ms,
                id,
                expected_revision
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        self.conn
            .query_row(
                "SELECT id,media_id,start_ms,end_ms,source_text,translated_text,ordinal,revision
                 FROM segments WHERE id=?1",
                [id],
                |r| {
                    Ok(Segment {
                        id: r.get(0)?,
                        media_id: r.get(1)?,
                        start_ms: r.get(2)?,
                        end_ms: r.get(3)?,
                        source_text: r.get(4)?,
                        translated_text: r.get(5)?,
                        ordinal: r.get(6)?,
                        revision: r.get(7)?,
                    })
                },
            )
            .optional()
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
}
