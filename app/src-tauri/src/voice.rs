//! Voice profiles for `speech.tts`.
//!
//! A Voice Profile is a reusable, user-owned description of *how* a dub line
//! should sound. Only two kinds exist in the dubbing pipeline:
//!
//! - `preset`: a voice the Provider ships (sent as `voice`);
//! - `design`: a text description of a synthetic voice (sent as
//!   `instructions` where the adapter supports it).
//!
//! There is deliberately no `clone` kind and no reference-audio field: the
//! pipeline never clones the original performer (docs/19 rule 5,
//! docs/09 "Real-person voices"). Params that would smuggle reference audio
//! into a request are rejected at validation time.
//!
//! Reference lineage: the VoiceProfile entity evolves the per-project
//! `tts_voice` / `tts_style_prompt` settings of ASMR-Dubber (MIT,
//! https://github.com/EveningStudy/asmr-dubber, commit `2faea94f`). No Dubber
//! code is copied here.

use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const VOICE_KINDS: &[&str] = &["preset", "design"];

/// Param keys that would turn a synthesis request into voice cloning. They are
/// rejected anywhere inside `params_json` (case-insensitive, nested objects
/// included).
const FORBIDDEN_PARAM_KEYS: &[&str] = &[
    "reference_audio",
    "ref_audio",
    "reference_wav",
    "speaker_wav",
    "prompt_audio",
    "prompt_wav",
    "voice_clone",
    "clone",
    "audio_prompt",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceProfile {
    pub id: String,
    pub name: String,
    /// `preset` or `design`.
    pub kind: String,
    pub provider_id: String,
    pub model_id: String,
    /// Provider voice id. Required for `preset`; optional base voice for
    /// `design` (some OpenAI-compatible endpoints require a `voice` field
    /// even when `instructions` carry the design).
    pub preset_voice: String,
    /// Text description of the voice. Required for `design`; must be empty for
    /// `preset`.
    pub design_prompt: String,
    /// BCP-47-ish language tag of the voice (`zh-Hans`, `en`, ...).
    pub language: String,
    /// Canonical JSON object of extra synthesis params (e.g. `{"speed":1.0}`).
    pub params_json: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct VoiceProfileInput {
    pub id: Option<String>,
    pub name: String,
    pub kind: String,
    pub provider_id: String,
    pub model_id: String,
    #[serde(default)]
    pub preset_voice: Option<String>,
    #[serde(default)]
    pub design_prompt: Option<String>,
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub params_json: Option<String>,
}

/// Creates `voice_profiles`. Idempotent; called from `MediaDb::open`.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS voice_profiles (
           id TEXT PRIMARY KEY,
           name TEXT NOT NULL,
           kind TEXT NOT NULL CHECK (kind IN ('preset','design')),
           provider_id TEXT NOT NULL,
           model_id TEXT NOT NULL,
           preset_voice TEXT NOT NULL DEFAULT '',
           design_prompt TEXT NOT NULL DEFAULT '',
           language TEXT NOT NULL DEFAULT '',
           params_json TEXT NOT NULL DEFAULT '{}',
           created_at TEXT NOT NULL DEFAULT (datetime('now')),
           updated_at TEXT NOT NULL DEFAULT (datetime('now'))
         );",
    )
}

/// Serializes JSON with object keys sorted recursively, so semantically equal
/// params always hash identically regardless of serde_json feature flags.
pub fn canonical_json(value: &Value) -> String {
    match value {
        Value::Object(map) => {
            let mut keys: Vec<&String> = map.keys().collect();
            keys.sort();
            let body = keys
                .into_iter()
                .map(|key| {
                    format!(
                        "{}:{}",
                        Value::String(key.clone()),
                        canonical_json(&map[key])
                    )
                })
                .collect::<Vec<_>>()
                .join(",");
            format!("{{{body}}}")
        }
        Value::Array(items) => format!(
            "[{}]",
            items
                .iter()
                .map(canonical_json)
                .collect::<Vec<_>>()
                .join(",")
        ),
        other => other.to_string(),
    }
}

fn contains_forbidden_key(value: &Value) -> Option<String> {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                let lowered = key.to_ascii_lowercase();
                if FORBIDDEN_PARAM_KEYS.contains(&lowered.as_str()) {
                    return Some(key.clone());
                }
                if let Some(found) = contains_forbidden_key(inner) {
                    return Some(found);
                }
            }
            None
        }
        Value::Array(items) => items.iter().find_map(contains_forbidden_key),
        _ => None,
    }
}

/// Parses and canonicalizes `params_json`. Must be a JSON object without
/// reference-audio / cloning keys.
pub fn normalize_params(raw: Option<&str>) -> Result<String, String> {
    let raw = raw.map(str::trim).unwrap_or("");
    if raw.is_empty() {
        return Ok("{}".into());
    }
    let value: Value =
        serde_json::from_str(raw).map_err(|e| format!("voice params are not valid JSON: {e}"))?;
    if !value.is_object() {
        return Err("voice params must be a JSON object".into());
    }
    if let Some(key) = contains_forbidden_key(&value) {
        return Err(format!(
            "voice params may not contain `{key}`: reference-audio voice cloning is not part of the dubbing pipeline"
        ));
    }
    Ok(canonical_json(&value))
}

/// Validates user input into the stored shape (without id/timestamps).
pub fn validate_input(input: &VoiceProfileInput) -> Result<VoiceProfileInput, String> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err("voice profile name is required".into());
    }
    let kind = input.kind.trim().to_ascii_lowercase();
    if !VOICE_KINDS.contains(&kind.as_str()) {
        return Err(format!(
            "voice profile kind must be preset or design (got `{}`); voice cloning is not supported",
            input.kind.trim()
        ));
    }
    let provider_id = input.provider_id.trim();
    let model_id = input.model_id.trim();
    if provider_id.is_empty() || model_id.is_empty() {
        return Err("voice profile requires a Provider and a Model".into());
    }
    let preset_voice = input.preset_voice.as_deref().unwrap_or("").trim();
    let design_prompt = input.design_prompt.as_deref().unwrap_or("").trim();
    match kind.as_str() {
        "preset" => {
            if preset_voice.is_empty() {
                return Err("a preset voice profile needs a preset voice id".into());
            }
            if !design_prompt.is_empty() {
                return Err("a preset voice profile cannot carry a design prompt".into());
            }
        }
        _ => {
            if design_prompt.is_empty() {
                return Err("a design voice profile needs a design prompt".into());
            }
        }
    }
    let params_json = normalize_params(input.params_json.as_deref())?;
    Ok(VoiceProfileInput {
        id: input
            .id
            .as_deref()
            .map(str::trim)
            .filter(|v| !v.is_empty())
            .map(str::to_string),
        name: name.into(),
        kind,
        provider_id: provider_id.into(),
        model_id: model_id.into(),
        preset_voice: Some(preset_voice.into()),
        design_prompt: Some(design_prompt.into()),
        language: input.language.trim().into(),
        params_json: Some(params_json),
    })
}

/// Hash of everything about the *voice* that changes the synthesized audio.
/// Name, id and timestamps are excluded, so renaming a profile never
/// invalidates the cache. Provider/model/language/params are separate cache
/// key components (see `tts::cache_key`).
pub fn voice_profile_hash(profile: &VoiceProfile) -> String {
    let design = if profile.kind == "design" {
        profile.design_prompt.trim()
    } else {
        ""
    };
    let identity = serde_json::json!({
        "v": 1,
        "kind": profile.kind,
        "preset_voice": profile.preset_voice.trim(),
        "design_prompt": design,
    });
    let mut hasher = Sha256::new();
    hasher.update(canonical_json(&identity).as_bytes());
    hex(&hasher.finalize())
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn row_to_profile(row: &Row<'_>) -> rusqlite::Result<VoiceProfile> {
    Ok(VoiceProfile {
        id: row.get(0)?,
        name: row.get(1)?,
        kind: row.get(2)?,
        provider_id: row.get(3)?,
        model_id: row.get(4)?,
        preset_voice: row.get(5)?,
        design_prompt: row.get(6)?,
        language: row.get(7)?,
        params_json: row.get(8)?,
        created_at: row.get(9)?,
        updated_at: row.get(10)?,
    })
}

const SELECT: &str = "SELECT id,name,kind,provider_id,model_id,preset_voice,design_prompt,
                             language,params_json,created_at,updated_at
                      FROM voice_profiles";

pub fn list(conn: &Connection) -> rusqlite::Result<Vec<VoiceProfile>> {
    let mut stmt = conn.prepare(&format!("{SELECT} ORDER BY name COLLATE NOCASE,id"))?;
    let rows = stmt.query_map([], row_to_profile)?.collect();
    rows
}

pub fn get(conn: &Connection, id: &str) -> rusqlite::Result<Option<VoiceProfile>> {
    conn.query_row(&format!("{SELECT} WHERE id=?1"), [id], row_to_profile)
        .optional()
}

/// Inserts or updates a profile after validation.
pub fn save(conn: &Connection, input: &VoiceProfileInput) -> Result<VoiceProfile, String> {
    let clean = validate_input(input)?;
    let id = match clean.id.clone() {
        Some(id) => {
            if get(conn, &id).map_err(|e| e.to_string())?.is_none() {
                return Err("voice profile not found".into());
            }
            conn.execute(
                "UPDATE voice_profiles
                 SET name=?1,kind=?2,provider_id=?3,model_id=?4,preset_voice=?5,
                     design_prompt=?6,language=?7,params_json=?8,updated_at=datetime('now')
                 WHERE id=?9",
                params![
                    clean.name,
                    clean.kind,
                    clean.provider_id,
                    clean.model_id,
                    clean.preset_voice,
                    clean.design_prompt,
                    clean.language,
                    clean.params_json,
                    id
                ],
            )
            .map_err(|e| e.to_string())?;
            id
        }
        None => {
            let id = format!("voice-{}", uuid::Uuid::new_v4());
            conn.execute(
                "INSERT INTO voice_profiles(
                    id,name,kind,provider_id,model_id,preset_voice,design_prompt,language,params_json
                 ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![
                    id,
                    clean.name,
                    clean.kind,
                    clean.provider_id,
                    clean.model_id,
                    clean.preset_voice,
                    clean.design_prompt,
                    clean.language,
                    clean.params_json
                ],
            )
            .map_err(|e| e.to_string())?;
            id
        }
    };
    get(conn, &id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "voice profile not found after save".into())
}

/// Deletes a profile. Synthesized assets stay in the content-addressed cache;
/// segments keep their `tts_asset_key`.
pub fn delete(conn: &Connection, id: &str) -> rusqlite::Result<bool> {
    Ok(conn.execute("DELETE FROM voice_profiles WHERE id=?1", [id])? > 0)
}

// ----- Tauri commands -----

#[tauri::command]
pub fn list_voice_profiles(
    state: tauri::State<crate::AppState>,
) -> Result<Vec<VoiceProfile>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    list(db.connection()).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn save_voice_profile(
    input: VoiceProfileInput,
    state: tauri::State<crate::AppState>,
) -> Result<VoiceProfile, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    save(db.connection(), &input)
}

#[tauri::command]
pub fn delete_voice_profile(
    id: String,
    state: tauri::State<crate::AppState>,
) -> Result<bool, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    delete(db.connection(), &id).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn
    }

    fn design_input() -> VoiceProfileInput {
        VoiceProfileInput {
            id: None,
            name: " Soft whisper ".into(),
            kind: "design".into(),
            provider_id: "prov".into(),
            model_id: "tts-model".into(),
            preset_voice: Some("alloy".into()),
            design_prompt: Some("温柔自然的中文女声，耳语感".into()),
            language: "zh-Hans".into(),
            params_json: Some(r#"{"speed":1.0,"a":{"z":1,"b":2}}"#.into()),
        }
    }

    #[test]
    fn crud_roundtrip_and_params_are_canonical() {
        let conn = conn();
        let saved = save(&conn, &design_input()).unwrap();
        assert_eq!(saved.name, "Soft whisper");
        assert_eq!(saved.params_json, r#"{"a":{"b":2,"z":1},"speed":1.0}"#);
        assert_eq!(list(&conn).unwrap().len(), 1);

        let mut update = design_input();
        update.id = Some(saved.id.clone());
        update.name = "Renamed".into();
        let updated = save(&conn, &update).unwrap();
        assert_eq!(updated.id, saved.id);
        assert_eq!(updated.name, "Renamed");
        // Renaming does not change the voice identity.
        assert_eq!(voice_profile_hash(&saved), voice_profile_hash(&updated));

        assert!(delete(&conn, &saved.id).unwrap());
        assert!(get(&conn, &saved.id).unwrap().is_none());
    }

    #[test]
    fn rejects_clone_kind_and_reference_audio_params() {
        let conn = conn();
        let mut clone = design_input();
        clone.kind = "clone".into();
        assert!(save(&conn, &clone).unwrap_err().contains("cloning"));

        let mut smuggled = design_input();
        smuggled.params_json = Some(r#"{"extra":{"Reference_Audio":"x.wav"}}"#.into());
        assert!(save(&conn, &smuggled)
            .unwrap_err()
            .contains("Reference_Audio"));

        let mut not_object = design_input();
        not_object.params_json = Some("[1,2]".into());
        assert!(save(&conn, &not_object).is_err());
    }

    #[test]
    fn kind_specific_fields_are_required() {
        let mut preset = design_input();
        preset.kind = "preset".into();
        assert!(
            validate_input(&preset).is_err(),
            "preset with design prompt"
        );
        preset.design_prompt = None;
        assert!(validate_input(&preset).is_ok());
        preset.preset_voice = Some("  ".into());
        assert!(validate_input(&preset).is_err());

        let mut design = design_input();
        design.design_prompt = Some(" ".into());
        assert!(validate_input(&design).is_err());
        design.design_prompt = Some("bright narrator".into());
        design.preset_voice = None;
        assert!(validate_input(&design).is_ok());
    }

    #[test]
    fn hash_tracks_voice_identity() {
        let conn = conn();
        let base = save(&conn, &design_input()).unwrap();
        let mut other = base.clone();
        other.design_prompt = "low calm male voice".into();
        assert_ne!(voice_profile_hash(&base), voice_profile_hash(&other));
        let mut other_voice = base.clone();
        other_voice.preset_voice = "verse".into();
        assert_ne!(voice_profile_hash(&base), voice_profile_hash(&other_voice));
    }
}
