mod alignment;
mod asr;
mod db;
mod providers;
mod refine;
mod subtitle;

use alignment::{AlignmentResult, AlignmentTarget};
use asr::{AsrResult, AsrRunInput};
use db::{MediaDb, MediaItem, ProposalDecision, RefineProposal, Segment};
use providers::{
    builtin_local_providers, builtin_provider, builtin_provider_models, delete_secret,
    discover_models, normalize_capabilities, presets, provider_target, read_secret, secret_ref,
    store_secret, CapabilityTarget, ModelDescriptor, ProviderConfig, ProviderInput, ProviderPreset,
    ProviderTestResult, CAPABILITY_VOCABULARY,
};
use refine::{GlossaryTerm, RefineSegmentInput, MAX_BATCH_SEGMENTS};
use serde::{Deserialize, Serialize};
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

    let provider_id = input.provider_id.trim().to_string();
    if provider_id.is_empty() {
        return Err("ASR requires provider_id".into());
    }
    let model_id = input.model_id.clone().unwrap_or_default();

    let result = if let Some(provider) = builtin_provider(&provider_id) {
        if !provider.enabled {
            return Err("selected local provider is disabled on this platform".into());
        }
        if provider.availability != "ready" {
            return Err(provider.message);
        }

        let allowed_models = builtin_provider_models(&provider_id);
        let default_model = allowed_models
            .first()
            .map(|model| model.model_id.clone())
            .unwrap_or_default();
        let resolved_model = if model_id.trim().is_empty() {
            default_model
        } else {
            model_id.clone()
        };
        if !allowed_models
            .iter()
            .any(|model| model.model_id == resolved_model)
        {
            return Err("model is not registered under the selected local provider".into());
        }

        match provider.kind.as_str() {
            "faster-whisper" => {
                let media_path = media.path.clone();
                let duration_ms = media.duration_ms;
                let language = input.language.clone();
                let prompt = input.prompt.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    asr::run_faster_whisper(
                        &media_path,
                        duration_ms,
                        &resolved_model,
                        language,
                        prompt,
                    )
                })
                .await
                .map_err(|e| e.to_string())??
            }
            "funasr-sensevoice" => {
                let media_path = media.path.clone();
                let duration_ms = media.duration_ms;
                let language = input.language.clone();
                let prompt = input.prompt.clone();
                tauri::async_runtime::spawn_blocking(move || {
                    asr::run_funasr(&media_path, duration_ms, &resolved_model, language, prompt)
                })
                .await
                .map_err(|e| e.to_string())??
            }
            "whisper-cpp" => {
                let media_path = media.path.clone();
                let language = input.language.clone();
                let logical_model = resolved_model.clone();
                let adapter_model = if logical_model == "runtime-model" {
                    String::new()
                } else {
                    logical_model.clone()
                };
                let mut result = tauri::async_runtime::spawn_blocking(move || {
                    asr::run_whisper_cpp(&media_path, &adapter_model, language)
                })
                .await
                .map_err(|e| e.to_string())??;
                result.model_id = logical_model;
                result
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
            other => return Err(format!("unsupported local provider adapter: {other}")),
        }
    } else {
        let model_id = input
            .model_id
            .as_deref()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| "remote/local-server ASR requires model_id".to_string())?;

        let provider = {
            let db = state.db.lock().map_err(|e| e.to_string())?;
            let provider = db
                .get_provider(&provider_id)
                .map_err(|e| e.to_string())?
                .ok_or_else(|| "ASR provider not found".to_string())?;
            if !provider.enabled {
                return Err("ASR provider is disabled".into());
            }
            let model = db
                .list_provider_models(&provider_id)
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

        if provider.kind != "openai-compatible" {
            return Err(format!(
                "provider adapter {} does not implement speech.asr yet",
                provider.kind
            ));
        }

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

    let provenance = format!("asr:{}:{}", provider_id, result.model_id);

    let segments = {
        let mut db = state.db.lock().map_err(|e| e.to_string())?;
        let confidences = result
            .segments
            .iter()
            .map(|segment| segment.confidence)
            .collect::<Vec<_>>();
        db.replace_asr_segments(media.id, &cues, &confidences, &provenance)
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
    let mut providers = state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .list_providers()
        .map_err(|e| e.to_string())?;
    providers.extend(builtin_local_providers());
    providers.sort_by(|a, b| {
        b.system_managed
            .cmp(&a.system_managed)
            .then_with(|| a.name.cmp(&b.name))
    });
    Ok(providers)
}

#[tauri::command]
fn save_provider(input: ProviderInput, state: State<AppState>) -> Result<ProviderConfig, String> {
    let name = input.name.trim();
    if name.is_empty() {
        return Err("provider name is required".into());
    }
    if input.kind != "openai-compatible" {
        return Err(
            "user-configured providers currently require the openai-compatible adapter".into(),
        );
    }
    if !matches!(input.execution.as_str(), "remote_api" | "local_server") {
        return Err("user-configured provider execution must be remote_api or local_server".into());
    }
    if input
        .id
        .as_deref()
        .is_some_and(|id| id.starts_with("local."))
    {
        return Err("system-managed local providers cannot be overwritten".into());
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
        execution: input.execution,
        system_managed: false,
        availability: "ready".into(),
        message: String::new(),
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
    if builtin_provider(&id).is_some() {
        return Err("system-managed local providers cannot be deleted".into());
    }
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
    if builtin_provider(&provider_id).is_some() {
        return Ok(builtin_provider_models(&provider_id));
    }
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

    for provider in builtin_local_providers()
        .into_iter()
        .filter(|provider| provider.enabled)
    {
        candidates.extend(
            builtin_provider_models(&provider.id)
                .into_iter()
                .filter(|model| {
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
fn resolve_capability_targets(
    capability: String,
    state: State<AppState>,
) -> Result<Vec<CapabilityTarget>, String> {
    let capability = capability.trim().to_ascii_lowercase();
    if !CAPABILITY_VOCABULARY.contains(&capability.as_str()) {
        return Err(format!("unknown capability: {capability}"));
    }

    let db = state.db.lock().map_err(|e| e.to_string())?;
    let mut targets = Vec::new();

    for provider in db
        .list_providers()
        .map_err(|e| e.to_string())?
        .into_iter()
        .filter(|provider| provider.enabled)
    {
        for model in db
            .list_provider_models(&provider.id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .filter(|model| model.effective_capabilities.contains(&capability))
        {
            targets.push(provider_target(&provider, &model));
        }
    }

    for provider in builtin_local_providers()
        .into_iter()
        .filter(|provider| provider.enabled)
    {
        for model in builtin_provider_models(&provider.id)
            .into_iter()
            .filter(|model| model.effective_capabilities.contains(&capability))
        {
            targets.push(provider_target(&provider, &model));
        }
    }

    targets.sort_by(|a, b| {
        a.provider_name
            .cmp(&b.provider_name)
            .then_with(|| a.model_id.cmp(&b.model_id))
    });
    Ok(targets)
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

// ----- transcript refinement / glossary / alignment (PR #9) -----

#[derive(Debug, Clone, Deserialize)]
struct GlossaryInput {
    id: Option<i64>,
    term: String,
    #[serde(default)]
    aliases: Vec<String>,
    #[serde(default)]
    note: String,
}

#[tauri::command]
fn list_glossary(state: State<AppState>) -> Result<Vec<GlossaryTerm>, String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .list_glossary()
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn save_glossary_term(
    input: GlossaryInput,
    state: State<AppState>,
) -> Result<GlossaryTerm, String> {
    if input.term.trim().is_empty() {
        return Err("glossary term cannot be empty".into());
    }
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .save_glossary_term(input.id, &input.term, &input.aliases, &input.note)
        .map_err(|e| {
            if e.to_string().contains("UNIQUE") {
                "this term is already in the glossary".to_string()
            } else {
                e.to_string()
            }
        })
}

#[tauri::command]
fn delete_glossary_term(id: i64, state: State<AppState>) -> Result<(), String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .delete_glossary_term(id)
        .map_err(|e| e.to_string())
}

#[derive(Debug, Clone, Deserialize)]
struct RefineRunInput {
    media_id: i64,
    provider_id: String,
    model_id: String,
    /// Limit the run to these Segments; all Segments of the media otherwise.
    segment_ids: Option<Vec<i64>>,
    language: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RefineOutcome {
    provenance: String,
    proposals: Vec<RefineProposal>,
    new_proposals: usize,
    checked_segments: usize,
    notes: Vec<String>,
}

#[tauri::command]
async fn refine_transcript(
    input: RefineRunInput,
    state: State<'_, AppState>,
) -> Result<RefineOutcome, String> {
    let provider_id = input.provider_id.trim().to_string();
    let model_id = input.model_id.trim().to_string();
    if provider_id.is_empty() || model_id.is_empty() {
        return Err("refinement requires a Provider and a Model".into());
    }

    let (provider, segments, glossary) = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let provider = db
            .get_provider(&provider_id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "refine Provider not found".to_string())?;
        if !provider.enabled {
            return Err("refine Provider is disabled".into());
        }
        let model = db
            .list_provider_models(&provider_id)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|model| model.model_id == model_id)
            .ok_or_else(|| "model not found in Provider inventory".to_string())?;
        if !model.available {
            return Err("model is currently unavailable".into());
        }
        if !model
            .effective_capabilities
            .iter()
            .any(|capability| capability == "transcript.refine")
        {
            return Err("selected model does not resolve transcript.refine".into());
        }
        let segments = db.get_segments(input.media_id).map_err(|e| e.to_string())?;
        let glossary = db.list_glossary().map_err(|e| e.to_string())?;
        (provider, segments, glossary)
    };

    if provider.kind != "openai-compatible" {
        return Err(format!(
            "provider adapter {} does not implement transcript.refine yet",
            provider.kind
        ));
    }

    let wanted = input.segment_ids.as_ref();
    let inputs = segments
        .iter()
        .filter(|segment| wanted.is_none_or(|ids| ids.contains(&segment.id)))
        .filter(|segment| !segment.source_text.trim().is_empty())
        .map(|segment| RefineSegmentInput {
            segment_id: segment.id,
            revision: segment.revision,
            start_ms: segment.start_ms,
            end_ms: segment.end_ms,
            text: segment.source_text.clone(),
        })
        .collect::<Vec<_>>();
    if inputs.is_empty() {
        return Err("there is no transcript to refine".into());
    }

    let secret = provider_secret(&provider)?;
    let mut accepted = Vec::new();
    let mut notes = Vec::new();
    let mut succeeded = 0usize;
    let mut last_error = None;
    let checked_segments = inputs.len();

    for batch in inputs.chunks(MAX_BATCH_SEGMENTS) {
        let request =
            refine::build_request(input.language.clone(), batch.to_vec(), glossary.clone());
        match refine::run_openai_compatible(&provider, secret.as_deref(), &model_id, &request).await
        {
            Ok(result) => {
                succeeded += 1;
                let report = refine::validate_proposals(&request, &result);
                accepted.extend(report.accepted);
                notes.extend(report.notes);
            }
            Err(error) => {
                notes.push(format!(
                    "A batch of {} lines failed and was left unchanged: {error}",
                    batch.len()
                ));
                last_error = Some(error);
            }
        }
    }
    if succeeded == 0 {
        return Err(last_error.unwrap_or_else(|| "refinement failed".into()));
    }

    let provenance = format!("refine:{provider_id}:{model_id}");
    let new_proposals = accepted.len();
    let proposals = {
        let mut db = state.db.lock().map_err(|e| e.to_string())?;
        db.insert_refine_proposals(input.media_id, &provenance, &accepted)
            .map_err(|e| e.to_string())?
    };

    Ok(RefineOutcome {
        provenance,
        proposals,
        new_proposals,
        checked_segments,
        notes,
    })
}

#[tauri::command]
fn list_refine_proposals(
    media_id: i64,
    include_resolved: Option<bool>,
    state: State<AppState>,
) -> Result<Vec<RefineProposal>, String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .list_refine_proposals(media_id, include_resolved.unwrap_or(false))
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn accept_refine_proposal(id: i64, state: State<AppState>) -> Result<ProposalDecision, String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .accept_refine_proposal(id)
        .map_err(|e| e.to_string())
}

#[tauri::command]
fn reject_refine_proposal(id: i64, state: State<AppState>) -> Result<ProposalDecision, String> {
    state
        .db
        .lock()
        .map_err(|e| e.to_string())?
        .reject_refine_proposal(id)
        .map_err(|e| e.to_string())
}

/// Applies a forced-alignment result produced by an aligner worker or agent.
/// The result is validated against current Segment revisions first.
#[tauri::command]
fn apply_alignment_result(
    result: AlignmentResult,
    state: State<AppState>,
) -> Result<Vec<Segment>, String> {
    let mut db = state.db.lock().map_err(|e| e.to_string())?;
    let media = db
        .get_media(result.media_id)
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "media not found".to_string())?;
    let targets = db
        .get_segments(media.id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .map(|segment| AlignmentTarget {
            segment_id: segment.id,
            revision: segment.revision,
        })
        .collect::<Vec<_>>();
    alignment::validate(&result, media.id, media.duration_ms, &targets)?;
    db.apply_alignment(&result)
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
            resolve_capability_targets,
            set_model_capabilities,
            list_glossary,
            save_glossary_term,
            delete_glossary_term,
            refine_transcript,
            list_refine_proposals,
            accept_refine_proposal,
            reject_refine_proposal,
            apply_alignment_result
        ])
        .run(tauri::generate_context!())
        .expect("error while running Tsubame");
}
