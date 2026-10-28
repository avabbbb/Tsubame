//! P7 offline word lookup.
//!
//! No dictionary data ships with YUZUKI. The user imports a file they
//! downloaded themselves and we index it into dictionary.db under the app
//! data dir (never redistributed):
//! - JMdict via jmdict-simplified JSON (EDRDG, CC BY-SA 4.0; attribution
//!   required in software that uses it, see JMDICT_ATTRIBUTION).
//! - ECDICT CSV (MIT).

use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::{Mutex, MutexGuard};

pub const JMDICT_ATTRIBUTION: &str = "This application uses the JMdict/EDICT dictionary files. These files are the property of the Electronic Dictionary Research and Development Group, and are used in conformance with the Group's licence (CC BY-SA 4.0)";
pub const ECDICT_ATTRIBUTION: &str =
    "English-Chinese entries from ECDICT by skywind3000 (MIT License)";

const MAX_GLOSS_CHARS: usize = 600;
const MAX_LOOKUP_CHARS: usize = 24;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DictKind {
    Jmdict,
    Ecdict,
}

impl DictKind {
    pub fn id(self) -> &'static str {
        match self {
            DictKind::Jmdict => "jmdict",
            DictKind::Ecdict => "ecdict",
        }
    }

    fn from_id(id: &str) -> Option<Self> {
        match id {
            "jmdict" => Some(DictKind::Jmdict),
            "ecdict" => Some(DictKind::Ecdict),
            _ => None,
        }
    }

    pub fn attribution(self) -> &'static str {
        match self {
            DictKind::Jmdict => JMDICT_ATTRIBUTION,
            DictKind::Ecdict => ECDICT_ATTRIBUTION,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Row {
    headword: String,
    reading: String,
    gloss: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DictEntry {
    pub source: String,
    pub headword: String,
    pub reading: String,
    pub gloss: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DictSource {
    pub source: String,
    pub entries: i64,
    pub imported_at: String,
    pub attribution: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct DictLookup {
    /// The dictionary form that matched (may be shorter than the query).
    pub matched: String,
    pub entries: Vec<DictEntry>,
}

pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS dict_entries (
           id INTEGER PRIMARY KEY AUTOINCREMENT,
           source TEXT NOT NULL,
           headword TEXT NOT NULL,
           reading TEXT NOT NULL DEFAULT '',
           gloss TEXT NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_dict_headword ON dict_entries(headword);
         CREATE INDEX IF NOT EXISTS idx_dict_reading ON dict_entries(reading);
         CREATE TABLE IF NOT EXISTS dict_sources (
           source TEXT PRIMARY KEY,
           entries INTEGER NOT NULL,
           imported_at TEXT NOT NULL
         );",
    )
}

fn db_err(err: rusqlite::Error) -> String {
    format!("dictionary database error: {err}")
}

pub struct DictStore {
    conn: Mutex<Connection>,
}

impl DictStore {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        migrate(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
        })
    }

    #[cfg(test)]
    fn open_in_memory() -> Self {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        Self {
            conn: Mutex::new(conn),
        }
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    pub fn import_file(&self, kind: DictKind, path: &Path) -> Result<DictSource, String> {
        let text = std::fs::read_to_string(path)
            .map_err(|err| format!("cannot read dictionary file {}: {err}", path.display()))?;
        let rows = match kind {
            DictKind::Jmdict => parse_jmdict(&text)?,
            DictKind::Ecdict => parse_ecdict(&text)?,
        };
        if rows.is_empty() {
            return Err("no dictionary entries found in this file".into());
        }
        self.replace_source(kind, &rows)
    }

    fn replace_source(&self, kind: DictKind, rows: &[Row]) -> Result<DictSource, String> {
        {
            let mut conn = self.lock();
            let tx = conn.transaction().map_err(db_err)?;
            tx.execute("DELETE FROM dict_entries WHERE source = ?1", [kind.id()])
                .map_err(db_err)?;
            {
                let mut stmt = tx
                    .prepare(
                        "INSERT INTO dict_entries (source, headword, reading, gloss)
                         VALUES (?1, ?2, ?3, ?4)",
                    )
                    .map_err(db_err)?;
                for row in rows {
                    stmt.execute(params![kind.id(), row.headword, row.reading, row.gloss])
                        .map_err(db_err)?;
                }
            }
            tx.execute(
                "INSERT INTO dict_sources (source, entries, imported_at)
                 VALUES (?1, ?2, datetime('now'))
                 ON CONFLICT(source) DO UPDATE SET
                   entries = excluded.entries, imported_at = excluded.imported_at",
                params![kind.id(), rows.len() as i64],
            )
            .map_err(db_err)?;
            tx.commit().map_err(db_err)?;
        }
        self.sources()?
            .into_iter()
            .find(|source| source.source == kind.id())
            .ok_or_else(|| "dictionary import was not recorded".to_string())
    }

    pub fn remove_source(&self, kind: DictKind) -> Result<(), String> {
        let mut conn = self.lock();
        let tx = conn.transaction().map_err(db_err)?;
        tx.execute("DELETE FROM dict_entries WHERE source = ?1", [kind.id()])
            .map_err(db_err)?;
        tx.execute("DELETE FROM dict_sources WHERE source = ?1", [kind.id()])
            .map_err(db_err)?;
        tx.commit().map_err(db_err)
    }

    pub fn sources(&self) -> Result<Vec<DictSource>, String> {
        let conn = self.lock();
        let mut stmt = conn
            .prepare("SELECT source, entries, imported_at FROM dict_sources ORDER BY source")
            .map_err(db_err)?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(db_err)?;
        let mut out = Vec::new();
        for row in rows {
            let (source, entries, imported_at) = row.map_err(db_err)?;
            let attribution = DictKind::from_id(&source)
                .map(|kind| kind.attribution().to_string())
                .unwrap_or_default();
            out.push(DictSource {
                source,
                entries,
                imported_at,
                attribution,
            });
        }
        Ok(out)
    }

    /// Exact match first, then the longest prefix of the query that is a
    /// headword or reading. For Japanese, a shortened stem is also tried with
    /// る / い restored, which covers most ichidan verbs and i-adjectives
    /// (食べました → 食べる, 高かった → 高い). This is not a full deinflector.
    pub fn lookup(&self, query: &str, limit: usize) -> Result<Option<DictLookup>, String> {
        let trimmed = query.trim_matches(|c: char| !c.is_alphanumeric());
        let chars: Vec<char> = trimmed.chars().take(MAX_LOOKUP_CHARS).collect();
        if chars.is_empty() {
            return Ok(None);
        }
        let ascii = chars.iter().all(char::is_ascii);
        let conn = self.lock();
        let mut stmt = conn
            .prepare(
                "SELECT source, headword, reading, gloss FROM dict_entries
                 WHERE headword = ?1 OR reading = ?1
                 ORDER BY CASE WHEN headword = ?1 THEN 0 ELSE 1 END, id
                 LIMIT ?2",
            )
            .map_err(db_err)?;
        let shortest = if ascii { chars.len() } else { 1 };
        for len in (shortest..=chars.len()).rev() {
            let prefix: String = chars[..len].iter().collect();
            let mut candidates = vec![prefix.clone()];
            if ascii {
                let lower = prefix.to_lowercase();
                if lower != prefix {
                    candidates.push(lower);
                }
            } else if len < chars.len() {
                candidates.push(format!("{prefix}る"));
                candidates.push(format!("{prefix}い"));
            }
            for candidate in candidates {
                let entries = stmt
                    .query_map(params![candidate, limit.max(1) as i64], |row| {
                        Ok(DictEntry {
                            source: row.get(0)?,
                            headword: row.get(1)?,
                            reading: row.get(2)?,
                            gloss: row.get(3)?,
                        })
                    })
                    .map_err(db_err)?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(db_err)?;
                if !entries.is_empty() {
                    return Ok(Some(DictLookup {
                        matched: candidate,
                        entries,
                    }));
                }
            }
        }
        Ok(None)
    }
}

fn clip(text: &str) -> String {
    if text.chars().count() <= MAX_GLOSS_CHARS {
        return text.to_string();
    }
    let mut out: String = text.chars().take(MAX_GLOSS_CHARS).collect();
    out.push('…');
    out
}

#[derive(Deserialize)]
struct JmRoot {
    words: Vec<JmWord>,
}

#[derive(Deserialize)]
struct JmWord {
    #[serde(default)]
    kanji: Vec<JmText>,
    #[serde(default)]
    kana: Vec<JmText>,
    #[serde(default)]
    sense: Vec<JmSense>,
}

#[derive(Deserialize)]
struct JmText {
    text: String,
}

#[derive(Deserialize)]
struct JmSense {
    #[serde(default)]
    gloss: Vec<JmText>,
}

/// jmdict-simplified JSON (jmdict-eng-*.json or any language variant).
fn parse_jmdict(text: &str) -> Result<Vec<Row>, String> {
    let root: JmRoot = serde_json::from_str(text)
        .map_err(|err| format!("not a jmdict-simplified JSON file: {err}"))?;
    let mut rows = Vec::new();
    for word in root.words {
        let senses: Vec<String> = word
            .sense
            .iter()
            .map(|sense| {
                sense
                    .gloss
                    .iter()
                    .map(|gloss| gloss.text.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .filter(|sense| !sense.is_empty())
            .collect();
        if senses.is_empty() {
            continue;
        }
        let gloss = if senses.len() == 1 {
            clip(&senses[0])
        } else {
            clip(
                &senses
                    .iter()
                    .enumerate()
                    .map(|(index, sense)| format!("{}. {sense}", index + 1))
                    .collect::<Vec<_>>()
                    .join("; "),
            )
        };
        let first_kana = word
            .kana
            .first()
            .map(|kana| kana.text.clone())
            .unwrap_or_default();
        if word.kanji.is_empty() {
            for kana in &word.kana {
                rows.push(Row {
                    headword: kana.text.clone(),
                    reading: String::new(),
                    gloss: gloss.clone(),
                });
            }
        } else {
            for kanji in &word.kanji {
                rows.push(Row {
                    headword: kanji.text.clone(),
                    reading: first_kana.clone(),
                    gloss: gloss.clone(),
                });
            }
        }
    }
    Ok(rows)
}

/// Streams CSV records (RFC 4180 quoting) to on_record without holding the
/// whole table in memory.
fn for_each_csv_record(text: &str, mut on_record: impl FnMut(&[String])) {
    let mut record: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut in_quotes = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if in_quotes {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    field.push('"');
                    chars.next();
                } else {
                    in_quotes = false;
                }
            } else {
                field.push(c);
            }
            continue;
        }
        match c {
            '"' => in_quotes = true,
            ',' => record.push(std::mem::take(&mut field)),
            '\n' => {
                record.push(std::mem::take(&mut field));
                on_record(&record);
                record.clear();
            }
            '\r' => {}
            _ => field.push(c),
        }
    }
    if !field.is_empty() || !record.is_empty() {
        record.push(field);
        on_record(&record);
    }
}

/// ECDICT ecdict.csv (header: word,phonetic,definition,translation,...).
fn parse_ecdict(text: &str) -> Result<Vec<Row>, String> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut columns: Option<(usize, usize, usize, usize)> = None;
    let mut header_error = None;
    let mut rows = Vec::new();
    for_each_csv_record(text, |record| {
        let Some((word, phonetic, definition, translation)) = columns else {
            let find = |name: &str| record.iter().position(|column| column.trim() == name);
            match (
                find("word"),
                find("phonetic"),
                find("definition"),
                find("translation"),
            ) {
                (Some(w), Some(p), Some(d), Some(t)) => columns = Some((w, p, d, t)),
                _ => header_error = Some(
                    "not an ECDICT CSV file (missing word/phonetic/definition/translation header)",
                ),
            }
            return;
        };
        if header_error.is_some() {
            return;
        }
        let cell = |index: usize| record.get(index).map(|value| value.trim()).unwrap_or("");
        let headword = cell(word);
        if headword.is_empty() {
            return;
        }
        let meaning = if cell(translation).is_empty() {
            cell(definition)
        } else {
            cell(translation)
        };
        if meaning.is_empty() {
            return;
        }
        rows.push(Row {
            headword: headword.to_string(),
            reading: cell(phonetic).to_string(),
            gloss: clip(&meaning.replace("\\n", "; ")),
        });
    });
    match header_error {
        Some(err) => Err(err.to_string()),
        None => Ok(rows),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_ecdict, parse_jmdict, DictKind, DictStore, Row};

    const JMDICT_SAMPLE: &str = r#"{
      "version": "3.6.1", "languages": ["eng"], "dictDate": "2026-10-19",
      "words": [
        {"id": "1358280", "kanji": [{"common": true, "text": "食べる", "tags": []}],
         "kana": [{"common": true, "text": "たべる", "tags": [], "appliesToKanji": [""]}],
         "sense": [{"partOfSpeech": ["v1"], "gloss": [{"lang": "eng", "text": "to eat"}]},
                   {"partOfSpeech": ["v1"], "gloss": [{"lang": "eng", "text": "to live on"}]}]},
        {"id": "1000320", "kanji": [],
         "kana": [{"common": true, "text": "ああ", "tags": []}],
         "sense": [{"gloss": [{"lang": "eng", "text": "like that"}, {"lang": "eng", "text": "so"}]}]}
      ]
    }"#;

    const ECDICT_SAMPLE: &str = "\u{feff}word,phonetic,definition,translation,pos\n\
        whisper,'wɪspə,\"n. a low voice, quiet\",\"n. 耳语\\nv. 低语\",\n\
        \"ear, \"\"inner\"\"\",,the organ of hearing,,\r\n";

    #[test]
    fn parses_jmdict_simplified_kanji_and_kana_only_words() {
        let rows = parse_jmdict(JMDICT_SAMPLE).unwrap();
        assert_eq!(
            rows,
            vec![
                Row {
                    headword: "食べる".into(),
                    reading: "たべる".into(),
                    gloss: "1. to eat; 2. to live on".into()
                },
                Row {
                    headword: "ああ".into(),
                    reading: String::new(),
                    gloss: "like that, so".into()
                },
            ]
        );
    }

    #[test]
    fn parses_ecdict_csv_with_quotes_and_falls_back_to_definition() {
        let rows = parse_ecdict(ECDICT_SAMPLE).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].headword, "whisper");
        assert_eq!(rows[0].reading, "'wɪspə");
        assert_eq!(rows[0].gloss, "n. 耳语; v. 低语");
        assert_eq!(rows[1].headword, "ear, \"inner\"");
        assert_eq!(rows[1].gloss, "the organ of hearing");
    }

    #[test]
    fn rejects_files_of_the_wrong_kind() {
        assert!(parse_ecdict("foo,bar\n1,2\n").is_err());
        assert!(parse_jmdict("{\"nope\": 1}").is_err());
    }

    #[test]
    fn lookup_matches_exact_reading_prefix_and_restored_stem() {
        let store = DictStore::open_in_memory();
        store
            .replace_source(DictKind::Jmdict, &parse_jmdict(JMDICT_SAMPLE).unwrap())
            .unwrap();
        store
            .replace_source(DictKind::Ecdict, &parse_ecdict(ECDICT_SAMPLE).unwrap())
            .unwrap();

        let exact = store.lookup("「食べる」", 5).unwrap().unwrap();
        assert_eq!(exact.matched, "食べる");
        assert_eq!(exact.entries[0].source, "jmdict");

        let by_reading = store.lookup("たべる", 5).unwrap().unwrap();
        assert_eq!(by_reading.entries[0].headword, "食べる");

        let inflected = store.lookup("食べました", 5).unwrap().unwrap();
        assert_eq!(inflected.matched, "食べる");

        let prefix = store.lookup("ああいう", 5).unwrap().unwrap();
        assert_eq!(prefix.matched, "ああ");

        let english = store.lookup("Whisper.", 5).unwrap().unwrap();
        assert_eq!(english.matched, "whisper");
        assert!(store.lookup("whispering", 5).unwrap().is_none());

        let sources = store.sources().unwrap();
        assert_eq!(sources.len(), 2);
        assert!(sources[1]
            .attribution
            .contains("Electronic Dictionary Research"));

        store.remove_source(DictKind::Jmdict).unwrap();
        assert!(store.lookup("食べる", 5).unwrap().is_none());
        assert_eq!(store.sources().unwrap().len(), 1);
    }
}
