use crate::subtitle::SubtitleCue;
use rusqlite::{params, Connection, OptionalExtension, Row};
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

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Segment {
    pub id: i64,
    pub media_id: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub source_text: String,
    pub translated_text: String,
    pub ordinal: i64,
    pub revision: i64,
    pub transcript_provenance: String,
    pub asr_provenance: String,
    pub refine_provenance: String,
    pub translation_provenance: String,
    pub tts_provenance: String,
    pub dirty_translation: bool,
    pub dirty_tts: bool,
    pub dirty_mix: bool,
    pub dirty_subtitle: bool,
    pub reviewed: bool,
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
               transcript_provenance TEXT NOT NULL DEFAULT '',
               asr_provenance TEXT NOT NULL DEFAULT '',
               refine_provenance TEXT NOT NULL DEFAULT '',
               translation_provenance TEXT NOT NULL DEFAULT '',
               tts_provenance TEXT NOT NULL DEFAULT '',
               dirty_translation INTEGER NOT NULL DEFAULT 0,
               dirty_tts INTEGER NOT NULL DEFAULT 0,
               dirty_mix INTEGER NOT NULL DEFAULT 0,
               dirty_subtitle INTEGER NOT NULL DEFAULT 0,
               reviewed INTEGER NOT NULL DEFAULT 0,
               FOREIGN KEY (media_id) REFERENCES media_files(id) ON DELETE CASCADE
             );
             CREATE INDEX IF NOT EXISTS idx_segments_media_time
               ON segments(media_id, start_ms, ordinal);",
        )?;

        Self::ensure_column(&conn, "media_files", "speed", "REAL NOT NULL DEFAULT 1.0")?;
        Self::ensure_column(&conn, "media_files", "volume", "REAL NOT NULL DEFAULT 1.0")?;

        for (name, definition) in [
            ("transcript_provenance", "TEXT NOT NULL DEFAULT ''"),
            ("asr_provenance", "TEXT NOT NULL DEFAULT ''"),
            ("refine_provenance", "TEXT NOT NULL DEFAULT ''"),
            ("translation_provenance", "TEXT NOT NULL DEFAULT ''"),
            ("tts_provenance", "TEXT NOT NULL DEFAULT ''"),
            ("dirty_translation", "INTEGER NOT NULL DEFAULT 0"),
            ("dirty_tts", "INTEGER NOT NULL DEFAULT 0"),
            ("dirty_mix", "INTEGER NOT NULL DEFAULT 0"),
            ("dirty_subtitle", "INTEGER NOT NULL DEFAULT 0"),
            ("reviewed", "INTEGER NOT NULL DEFAULT 0"),
        ] {
            Self::ensure_column(&conn, "segments", name, definition)?;
        }

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
             ON CONFLICT(path) DO UPDATE SET
               title=excluded.title,
               media_type=excluded.media_type,
               file_size=excluded.file_size",
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
                |row| {
                    Ok(MediaItem {
                        id: row.get(0)?,
                        path: row.get(1)?,
                        title: row.get(2)?,
                        media_type: row.get(3)?,
                        duration_ms: row.get(4)?,
                        playback_position: row.get(5)?,
                        file_size: row.get(6)?,
                        speed: row.get(7)?,
                        volume: row.get(8)?,
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

        stmt.query_map([], |row| {
            Ok(MediaItem {
                id: row.get(0)?,
                path: row.get(1)?,
                title: row.get(2)?,
                media_type: row.get(3)?,
                duration_ms: row.get(4)?,
                playback_position: row.get(5)?,
                file_size: row.get(6)?,
                speed: row.get(7)?,
                volume: row.get(8)?,
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
            "SELECT id,media_id,start_ms,end_ms,source_text,translated_text,ordinal,revision,
                    transcript_provenance,asr_provenance,refine_provenance,
                    translation_provenance,tts_provenance,
                    dirty_translation,dirty_tts,dirty_mix,dirty_subtitle,reviewed
             FROM segments
             WHERE media_id=?1
             ORDER BY start_ms,ordinal,id",
        )?;

        stmt.query_map([media_id], row_to_segment)?.collect()
    }

    pub fn get_segment(&self, id: i64) -> rusqlite::Result<Option<Segment>> {
        self.conn
            .query_row(
                "SELECT id,media_id,start_ms,end_ms,source_text,translated_text,ordinal,revision,
                        transcript_provenance,asr_provenance,refine_provenance,
                        translation_provenance,tts_provenance,
                        dirty_translation,dirty_tts,dirty_mix,dirty_subtitle,reviewed
                 FROM segments WHERE id=?1",
                [id],
                row_to_segment,
            )
            .optional()
    }

    pub fn replace_segments(
        &mut self,
        media_id: i64,
        cues: &[SubtitleCue],
        provenance: &str,
    ) -> rusqlite::Result<Vec<Segment>> {
        let tx = self.conn.transaction()?;
        tx.execute("DELETE FROM segments WHERE media_id=?1", [media_id])?;

        for (ordinal, cue) in cues.iter().enumerate() {
            tx.execute(
                "INSERT INTO segments(
                    media_id,start_ms,end_ms,source_text,translated_text,ordinal,revision,
                    transcript_provenance,dirty_translation,dirty_tts,dirty_mix,dirty_subtitle
                 ) VALUES(?1,?2,?3,?4,'',?5,0,?6,1,1,1,0)",
                params![
                    media_id,
                    cue.start_ms,
                    cue.end_ms,
                    cue.text,
                    ordinal as i64,
                    provenance
                ],
            )?;
        }
        tx.commit()?;
        self.get_segments(media_id)
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
        let Some(current) = self.get_segment(id)? else {
            return Ok(None);
        };
        if current.revision != expected_revision {
            return Ok(None);
        }

        let source_changed = current.source_text != source_text;
        let translation_changed = current.translated_text != translated_text;
        let timing_changed = current.start_ms != start_ms || current.end_ms != end_ms;
        let any_changed = source_changed || translation_changed || timing_changed;

        if !any_changed {
            return Ok(Some(current));
        }

        let dirty_translation = if translation_changed {
            false
        } else {
            current.dirty_translation || source_changed
        };
        let dirty_tts = current.dirty_tts || source_changed || translation_changed;
        let dirty_mix =
            current.dirty_mix || source_changed || translation_changed || timing_changed;
        let dirty_subtitle =
            current.dirty_subtitle || source_changed || translation_changed || timing_changed;
        let transcript_provenance = if source_changed {
            "human"
        } else {
            current.transcript_provenance.as_str()
        };
        let translation_provenance = if translation_changed {
            "human"
        } else {
            current.translation_provenance.as_str()
        };

        let changed = self.conn.execute(
            "UPDATE segments
             SET source_text=?1,
                 translated_text=?2,
                 start_ms=?3,
                 end_ms=?4,
                 revision=revision+1,
                 transcript_provenance=?5,
                 translation_provenance=?6,
                 dirty_translation=?7,
                 dirty_tts=?8,
                 dirty_mix=?9,
                 dirty_subtitle=?10,
                 reviewed=1
             WHERE id=?11 AND revision=?12",
            params![
                source_text,
                translated_text,
                start_ms,
                end_ms,
                transcript_provenance,
                translation_provenance,
                dirty_translation,
                dirty_tts,
                dirty_mix,
                dirty_subtitle,
                id,
                expected_revision
            ],
        )?;

        if changed == 0 {
            return Ok(None);
        }
        self.get_segment(id)
    }
}

fn row_to_segment(row: &Row<'_>) -> rusqlite::Result<Segment> {
    Ok(Segment {
        id: row.get(0)?,
        media_id: row.get(1)?,
        start_ms: row.get(2)?,
        end_ms: row.get(3)?,
        source_text: row.get(4)?,
        translated_text: row.get(5)?,
        ordinal: row.get(6)?,
        revision: row.get(7)?,
        transcript_provenance: row.get(8)?,
        asr_provenance: row.get(9)?,
        refine_provenance: row.get(10)?,
        translation_provenance: row.get(11)?,
        tts_provenance: row.get(12)?,
        dirty_translation: row.get(13)?,
        dirty_tts: row.get(14)?,
        dirty_mix: row.get(15)?,
        dirty_subtitle: row.get(16)?,
        reviewed: row.get(17)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup() -> rusqlite::Result<(MediaDb, MediaItem)> {
        let db = MediaDb::open(Path::new(":memory:"))?;
        let media = db.import_media("example.mp3")?;
        Ok((db, media))
    }

    #[test]
    fn playback_state_roundtrip() -> rusqlite::Result<()> {
        let (db, media) = setup()?;
        db.save_playback_state(media.id, 42_000, 1.25, 0.65)?;
        let row = db.list_media()?.remove(0);
        assert_eq!(row.playback_position, 42_000);
        assert!((row.speed - 1.25).abs() < f64::EPSILON);
        assert!((row.volume - 0.65).abs() < f64::EPSILON);
        Ok(())
    }

    #[test]
    fn subtitle_import_creates_canonical_segments() -> rusqlite::Result<()> {
        let (mut db, media) = setup()?;
        let cues = vec![
            SubtitleCue {
                start_ms: 1_000,
                end_ms: 2_000,
                text: "一行目".into(),
            },
            SubtitleCue {
                start_ms: 2_500,
                end_ms: 4_000,
                text: "二行目".into(),
            },
        ];
        let rows = db.replace_segments(media.id, &cues, "import:srt")?;
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].transcript_provenance, "import:srt");
        assert!(rows[0].dirty_translation);
        assert!(rows[0].dirty_tts);
        assert!(!rows[0].dirty_subtitle);
        Ok(())
    }

    #[test]
    fn translation_edit_does_not_dirty_translation_itself() -> rusqlite::Result<()> {
        let (mut db, media) = setup()?;
        let rows = db.replace_segments(
            media.id,
            &[SubtitleCue {
                start_ms: 1_000,
                end_ms: 2_000,
                text: "おやすみ".into(),
            }],
            "import:srt",
        )?;
        let original = &rows[0];
        let updated = db
            .update_segment(
                original.id,
                original.revision,
                &original.source_text,
                "晚安",
                original.start_ms,
                original.end_ms,
            )?
            .unwrap();

        assert!(!updated.dirty_translation);
        assert!(updated.dirty_tts);
        assert!(updated.dirty_mix);
        assert!(updated.dirty_subtitle);
        assert_eq!(updated.translation_provenance, "human");
        Ok(())
    }

    #[test]
    fn timing_edit_does_not_force_tts_when_tts_was_clean() -> rusqlite::Result<()> {
        let (mut db, media) = setup()?;
        let rows = db.replace_segments(
            media.id,
            &[SubtitleCue {
                start_ms: 1_000,
                end_ms: 2_000,
                text: "おやすみ".into(),
            }],
            "import:srt",
        )?;
        db.conn.execute(
            "UPDATE segments SET dirty_tts=0,dirty_mix=0,dirty_subtitle=0 WHERE id=?1",
            [rows[0].id],
        )?;
        let original = db.get_segment(rows[0].id)?.unwrap();
        let updated = db
            .update_segment(
                original.id,
                original.revision,
                &original.source_text,
                &original.translated_text,
                1_100,
                2_100,
            )?
            .unwrap();

        assert!(!updated.dirty_tts);
        assert!(updated.dirty_mix);
        assert!(updated.dirty_subtitle);
        Ok(())
    }
}
