mod asr;
mod db;
mod providers;
mod subtitle;

use asr::{AsrEngineDescriptor, AsrResult, AsrRunInput};
use db::{MediaDb, MediaItem, Segment};
use providers::{
    delete_secret, discover_models, normalize_capabilities, presets, read_secret, secret_ref,
    store_secret, ModelDescriptor, ProviderConfig, ProviderInput, ProviderPreset,
    ProviderTestResult, CAPABILITY_VOCABULARY,
};
use serde::Serialize;
use std::{fs, path::Path, sync::Mutex};
use subtitle::{SubtitleCue, SubtitleFormat};
use tauri::{Manager, State};

struct AppState {
    db: Mutex<MediaDb>,
}

#[tauri::command]
fn list_media(state: State<AppState>) -> Result<Vec<MediaItem>, String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .list_media()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn import_media(path: String, state: State<AppState>) -> Result<MediaItem, String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .import_media(&path)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn update_media_duration(id: i64, duration_ms: i64, state: State<AppState>) -> Result<(), String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .update_media_duration(id, duration_ms)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn save_playback_state(
    id: i64,
    position_ms: i64,
    speed: f64,
    volume: f64,
    state: State<AppState>,
) -> Result<(), String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .save_playback_state(id, position_ms, speed, volume)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_segments(media_id: i64, state: State<AppState>) -> Result<Vec<Segment>, String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .get_segments(media_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn get_segment(id: i64, state: State<AppState>) -> Result<Option<Segment>, String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .get_segment(id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn update_segment(
    id: i64,
    expected_revision: i64,
    source_text: String,
    translated_text: String,
    start_ms: i64,
    end_ms: i64,
    state: State<AppState>,
) -> Result<Segment, String> {
    if start_ms < 0 {
        return Err("segment start must be non-negative".to_string());
    }
    if end_ms <= start_ms {
        return Err("segment end must be after start".to_string());
    }

    let db = state.db.lock().map_err(|e| e.to_string())?;
    db.update_segment(
        id,
        expected_revision,
        &source_text,
        &translated_text,
        start_ms,
        end_ms,
    )
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "segment revision conflict".to_string())
}

#[tauri::command]
fn import_subtitles(
    media_id: i64,
    path: String,
    state: State<AppState>,
) -> Result<Vec<Segment>, String> {
    let path_ref = Path::new(&path);
    let format = SubtitleFormat::from_path(path_ref)?;
    let content = fs::read_to_string(path_ref).map_err(|e| e.to_string())?;
    let cues = subtitle::parse(&content, format)?;

    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .replace_segments(media_id, &cues, &format!("import:{}", format.label()))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn export_subtitles(
    media_id: i64,
    path: String,
    text_mode: String,
    state: State<AppState>,
) -> Result<String, String> {
    let path_ref = Path::new(&path);
    let format = SubtitleFormat::from_path(path_ref)?;
    let rows = state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .get_segments(media_id)
        .map_err(|e| e.to_string())?;

    if rows.is_empty() {
        return Err("track contains no segments".to_string());
    }

    let cues = rows
        .iter()
        .map(|segment| {
            let text = match text_mode.as_str() {
                "translation" => {
                    if segment.translated_text.trim().is_empty() {
                        segment.source_text.clone()
                    } else {
                        segment.translated_text.clone()
                    }
                }
                "bilingual" => {
                    if segment.translated_text.trim().is_empty() {
                        segment.source_text.clone()
                    } else {
                        format!("{}\n{}", segment.source_text, segment.translated_text)
                    }
                }
                _ => segment.source_text.clone(),
            };
            SubtitleCue {
                start_ms: segment.start_ms,
                end_ms: segment.end_ms,
                text,
            }
        })
        .collect::<Vec<_>>();

    let output = subtitle::serialize(&cues, format);
    fs::write(path_ref, output).map_err(|e| e.to_string())?;
    Ok(path)
}

#[derive(Debug, Clone, Serialize)]
struct AsrTranscriptionOutcome {
    result: AsrResult,
    segments: Vec<Segment>,
}

#[tauri::command]
fn list_asr_engines(state: State<AppState>) -> Result<Vec<AsrEngineDescriptor>, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    let remote_ready = db
        .list_providers()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|provider| provider.enabled)
        .any(|provider| {
            db.list_provider_models(&provider.id)
                .map(|models| {
                    models.into_iter().any(|model| {
                        model.available
                            && model
                                .effective_capabilities
                                .iter()
                                .any(|capability| capability == "speech.asr")
                    })
                })
                .unwrap_or(false)
        });
    Ok(asr::engines(remote_ready))
}

#[tauri::command]
async fn transcribe_media(
    input: AsrRunInput,
    state: State<'_, AppState>,
) -> Result<AsrTranscriptionOutcome, String> {
    let media = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        db.get_media(input.media_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "media not found".to_string())?
    };

    if media.media_type != "audio" {
        return Err(
            "ASR currently expects an audio asset; video audio extraction lands in Runtime Bootstrap"
                .into(),
        );
    }

    let model_id = input.model_id.clone().unwrap_or_default();
    let result = match input.engine_id.as_str() {
        "remote-openai-compatible" => {
            let provider_id = input
                .provider_id
                .as_deref()
                .ok_or_else(|| "remote ASR requires provider_id".to_string())?;
            let model_id = input
                .model_id
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| "remote ASR requires model_id".to_string())?;

            let provider = {
                let db = state.db.lock().map_err(|e| e.to_string())?;
                let provider = db
                    .get_provider(provider_id)
                    .map_err(|e| e.to_string())?
                    .ok_or_else(|| "ASR provider not found".to_string())?;
                if !provider.enabled {
                    return Err("ASR provider is disabled".into());
                }
                let model = db
                    .list_provider_models(provider_id)
                    .map_err(|e| e.to_string())?
                    .into_iter()
                    .find(|model| model.model_id == model_id)
                    .ok_or_else(|| "ASR model not found in provider inventory".to_string())?;
                if !model.available {
                    return Err("ASR model is currently unavailable".into());
                }
                if !model
                    .effective_capabilities
                    .iter()
                    .any(|capability| capability == "speech.asr")
                {
                    return Err("selected model does not resolve speech.asr".into());
                }
                provider
            };

            let secret = provider_secret(&provider)?;
            asr::run_remote_openai_compatible(
                &provider,
                secret.as_deref(),
                &media.path,
                media.duration_ms,
                model_id,
                input.language.clone(),
                input.prompt.clone(),
            )
            .await?
        }
        "faster-whisper" => {
            let media_path = media.path.clone();
            let duration_ms = media.duration_ms;
            let model = if model_id.trim().is_empty() {
                "large-v3".to_string()
            } else {
                model_id
            };
            let language = input.language.clone();
            let prompt = input.prompt.clone();
            tauri::async_runtime::spawn_blocking(move || {
                asr::run_faster_whisper(&media_path, duration_ms, &model, language, prompt)
            })
            .await
            .map_err(|e| e.to_string())??
        }
        "funasr-sensevoice" => {
            let media_path = media.path.clone();
            let duration_ms = media.duration_ms;
            let model = if model_id.trim().is_empty() {
                "iic/SenseVoiceSmall".to_string()
            } else {
                model_id
            };
            let language = input.language.clone();
            let prompt = input.prompt.clone();
            tauri::async_runtime::spawn_blocking(move || {
                asr::run_funasr(&media_path, duration_ms, &model, language, prompt)
            })
            .await
            .map_err(|e| e.to_string())??
        }
        "whisper-cpp" => {
            let media_path = media.path.clone();
            let model = model_id.clone();
            let language = input.language.clone();
            tauri::async_runtime::spawn_blocking(move || {
                asr::run_whisper_cpp(&media_path, &model, language)
            })
            .await
            .map_err(|e| e.to_string())??
        }
        "apple-speech" => {
            let media_path = media.path.clone();
            let duration_ms = media.duration_ms;
            let language = input.language.clone();
            tauri::async_runtime::spawn_blocking(move || {
                asr::run_apple_speech(&media_path, duration_ms, language)
            })
            .await
            .map_err(|e| e.to_string())??
        }
        other => return Err(format!("unknown ASR engine: {other}")),
    };

    asr::validate_result(&result)?;
    let cues = result
        .segments
        .iter()
        .map(|segment| SubtitleCue {
            start_ms: segment.start_ms,
            end_ms: segment.end_ms,
            text: segment.text.clone(),
        })
        .collect::<Vec<_>>();

    let provenance = format!(
        "asr:{}:{}:{}",
        result.engine_id,
        input.provider_id.as_deref().unwrap_or("local"),
        result.model_id
    );

    let segments = {
        let mut db = state.db.lock().map_err(|e| e.to_string())?;
        db.replace_asr_segments(media.id, &cues, &provenance)
            .map_err(|e| e.to_string())?
    };

    Ok(AsrTranscriptionOutcome { result, segments })
}

#[tauri::command]
fn provider_presets() -> Vec<ProviderPreset> {
    presets()
}

#[tauri::command]
fn provider_capabilities() -> Vec<String> {
    CAPABILITY_VOCABULARY
        .iter()
        .map(|value| (*value).to_string())
        .collect()
}

#[tauri::command]
fn list_providers(state: State<AppState>) -> Result<Vec<ProviderConfig>, String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .list_providers()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn save_provider(input: ProviderInput, state: State<AppState>) -> Result<ProviderConfig, String> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err("provider name is required".into());
    }
    if input.kind != "openai-compatible" {
        return Err("only openai-compatible remote providers are supported in this PR".into());
    }
    if !matches!(input.auth_mode.as_str(), "bearer" | "none") {
        return Err("provider auth_mode must be bearer or none".into());
    }

    let id = input
        .id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    let existing = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        db.get_provider(&id).map_err(|e| e.to_string())?
    };

    let mut next_secret_ref = existing
        .as_ref()
        .and_then(|provider| provider.secret_ref.clone());

    if input.auth_mode == "none" {
        if let Some(reference) = next_secret_ref.take() {
            delete_secret(&reference);
        }
    } else if let Some(api_key) = input.api_key.as_deref() {
        let trimmed = api_key.trim();
        if trimmed.is_empty() {
            if let Some(reference) = next_secret_ref.take() {
                delete_secret(&reference);
            }
        } else {
            let reference = secret_ref(&id);
            store_secret(&reference, trimmed)?;
            next_secret_ref = Some(reference);
        }
    }

    let provider = ProviderConfig {
        id: id.clone(),
        name: name.to_string(),
        kind: input.kind,
        base_url: input.base_url.trim().trim_end_matches('/').to_string(),
        model_list_url: input.model_list_url.trim().to_string(),
        auth_mode: input.auth_mode,
        secret_ref: next_secret_ref,
        enabled: input.enabled,
        last_refresh_at: existing
            .as_ref()
            .and_then(|provider| provider.last_refresh_at.clone()),
        last_error: existing.and_then(|provider| provider.last_error),
    };

    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .save_provider(&provider)
        .map_err(|e| e.to_string())?;

    Ok(provider)
}

#[tauri::command]
fn delete_provider(id: String, state: State<AppState>) -> Result<(), String> {
    let secret = state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .delete_provider(&id)
        .map_err(|e| e.to_string())?;
    if let Some(reference) = secret {
        delete_secret(&reference);
    }
    Ok(())
}

fn provider_secret(provider: &ProviderConfig) -> Result<Option<String>, String> {
    match provider.auth_mode.as_str() {
        "none" => Ok(None),
        "bearer" => {
            let reference = provider
                .secret_ref
                .as_deref()
                .ok_or_else(|| "provider API key is not configured".to_string())?;
            Ok(Some(read_secret(reference)?))
        }
        other => Err(format!("unsupported provider auth mode: {other}")),
    }
}

#[tauri::command]
async fn test_provider_connection(
    id: String,
    state: State<'_, AppState>,
) -> Result<ProviderTestResult, String> {
    let provider = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        db.get_provider(&id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "provider not found".to_string())?
    };
    let secret = provider_secret(&provider)?;
    Ok(providers::test_provider(&provider, secret.as_deref()).await)
}

#[tauri::command]
async fn refresh_provider_models(
    id: String,
    state: State<'_, AppState>,
) -> Result<Vec<ModelDescriptor>, String> {
    let provider = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        db.get_provider(&id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "provider not found".to_string())?
    };
    let secret = provider_secret(&provider)?;

    match discover_models(&provider, secret.as_deref()).await {
        Ok(discovered) => {
            let mut db = state.db.lock().map_err(|e| e.to_string())?;
            let models = db
                .replace_discovered_models(&id, &discovered)
                .map_err(|e| e.to_string())?;
            db.set_provider_refresh_status(&id, true, None)
                .map_err(|e| e.to_string())?;
            Ok(models)
        }
        Err(error) => {
            let db = state.db.lock().map_err(|e| e.to_string())?;
            let _ = db.set_provider_refresh_status(&id, false, Some(&error));
            Err(error)
        }
    }
}

#[tauri::command]
fn list_provider_models(
    provider_id: String,
    state: State<AppState>,
) -> Result<Vec<ModelDescriptor>, String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .list_provider_models(&provider_id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn resolve_capability(
    capability: String,
    state: State<AppState>,
) -> Result<Vec<ModelDescriptor>, String> {
    let capability = capability.trim().to_ascii_lowercase();
    if !CAPABILITY_VOCABULARY.contains(&capability.as_str()) {
        return Err(format!("unknown capability: {capability}"));
    }

    let db = state.db.lock().map_err(|e| e.to_string())?;
    let providers = db.list_providers().map_err(|e| e.to_string())?;
    let mut candidates = Vec::new();

    for provider in providers.into_iter().filter(|provider| provider.enabled) {
        let models = db
            .list_provider_models(&provider.id)
            .map_err(|e| e.to_string())?;
        candidates.extend(
            models.into_iter().filter(|model| {
                model.available && model.effective_capabilities.contains(&capability)
            }),
        );
    }

    candidates.sort_by(|a, b| {
        a.provider_id
            .cmp(&b.provider_id)
            .then_with(|| a.model_id.cmp(&b.model_id))
    });
    Ok(candidates)
}

#[tauri::command]
fn set_model_capabilities(
    provider_id: String,
    model_id: String,
    capabilities: Option<Vec<String>>,
    state: State<AppState>,
) -> Result<ModelDescriptor, String> {
    let normalized = capabilities
        .as_ref()
        .map(|values| normalize_capabilities(values));
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .set_model_capabilities(&provider_id, &model_id, normalized.as_ref())
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "provider model not found".to_string())
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            let dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&dir)?;
            let db = MediaDb::open(&dir.join("tsubame.db"))?;
            app.manage(AppState { db: Mutex::new(db) });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            list_media,
            import_media,
            update_media_duration,
            save_playback_state,
            get_segments,
            get_segment,
            update_segment,
            import_subtitles,
            export_subtitles,
            list_asr_engines,
            transcribe_media,
            provider_presets,
            provider_capabilities,
            list_providers,
            save_provider,
            delete_provider,
            test_provider_connection,
            refresh_provider_models,
            list_provider_models,
            resolve_capability,
            set_model_capabilities
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tsubame");
}
