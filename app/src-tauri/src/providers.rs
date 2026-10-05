use keyring::Entry;
use reqwest::{Client, StatusCode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

pub const KEYRING_SERVICE: &str = "dev.tsubame.provider";

pub const CAPABILITY_VOCABULARY: &[&str] = &[
    "audio.segment",
    "speech.asr",
    "transcript.refine",
    "speech.align",
    "text.generate",
    "text.translate",
    "speech.tts",
    "speech.voice_clone",
    "audio.separate",
    "speaker.diarize",
    "audio.mix",
    "embedding.text",
    "image.generate",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProviderConfig {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub model_list_url: String,
    pub auth_mode: String,
    pub secret_ref: Option<String>,
    pub enabled: bool,
    pub last_refresh_at: Option<String>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInput {
    pub id: Option<String>,
    pub name: String,
    pub kind: String,
    pub base_url: String,
    pub model_list_url: String,
    pub auth_mode: String,
    pub api_key: Option<String>,
    pub enabled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ModelDescriptor {
    pub provider_id: String,
    pub model_id: String,
    pub display_name: String,
    pub owned_by: String,
    pub available: bool,
    pub discovered_capabilities: Vec<String>,
    pub manual_capabilities: Option<Vec<String>>,
    pub effective_capabilities: Vec<String>,
    pub capability_source: String,
    pub last_seen_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiscoveredModel {
    pub model_id: String,
    pub display_name: String,
    pub owned_by: String,
    pub capabilities: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderPreset {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: &'static str,
    pub base_url: &'static str,
    pub model_list_url: &'static str,
    pub auth_mode: &'static str,
    pub note: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ProviderTestResult {
    pub ok: bool,
    pub status: String,
    pub model_count: usize,
    pub message: String,
}

pub fn presets() -> Vec<ProviderPreset> {
    vec![
        ProviderPreset {
            id: "openai",
            name: "OpenAI",
            kind: "openai-compatible",
            base_url: "https://api.openai.com/v1",
            model_list_url: "https://api.openai.com/v1/models",
            auth_mode: "bearer",
            note: "Official OpenAI API.",
        },
        ProviderPreset {
            id: "alibaba-model-studio-sg",
            name: "Alibaba Model Studio · Singapore",
            kind: "openai-compatible",
            base_url: "https://dashscope-intl.aliyuncs.com/compatible-mode/v1",
            model_list_url: "https://dashscope-intl.aliyuncs.com/api/v1/models",
            auth_mode: "bearer",
            note: "Editable defaults. Workspace-specific endpoints can be pasted over these values.",
        },
        ProviderPreset {
            id: "custom-openai-compatible",
            name: "Custom OpenAI-compatible",
            kind: "openai-compatible",
            base_url: "",
            model_list_url: "",
            auth_mode: "bearer",
            note: "Use any compatible endpoint. Model discovery URL can differ from inference base URL.",
        },
        ProviderPreset {
            id: "local-openai-compatible",
            name: "Local OpenAI-compatible",
            kind: "openai-compatible",
            base_url: "http://127.0.0.1:11434/v1",
            model_list_url: "http://127.0.0.1:11434/v1/models",
            auth_mode: "none",
            note: "Convenience preset for local servers that expose OpenAI-compatible model listing.",
        },
    ]
}

pub fn secret_ref(provider_id: &str) -> String {
    format!("tsubame/provider/{provider_id}/api-key")
}

pub fn store_secret(reference: &str, secret: &str) -> Result<(), String> {
    let entry = Entry::new(KEYRING_SERVICE, reference).map_err(|e| e.to_string())?;
    entry.set_password(secret).map_err(|e| e.to_string())
}

pub fn read_secret(reference: &str) -> Result<String, String> {
    let entry = Entry::new(KEYRING_SERVICE, reference).map_err(|e| e.to_string())?;
    entry.get_password().map_err(|e| e.to_string())
}

pub fn delete_secret(reference: &str) {
    if let Ok(entry) = Entry::new(KEYRING_SERVICE, reference) {
        let _ = entry.delete_credential();
    }
}

pub fn normalize_capabilities(values: &[String]) -> Vec<String> {
    let mut normalized = values
        .iter()
        .map(|value| value.trim().to_ascii_lowercase())
        .filter(|value| CAPABILITY_VOCABULARY.contains(&value.as_str()))
        .collect::<Vec<_>>();
    normalized.sort();
    normalized.dedup();
    normalized
}

pub fn resolve_capabilities(
    discovered: &[String],
    manual: Option<&Vec<String>>,
) -> (Vec<String>, String) {
    if let Some(manual) = manual {
        return (normalize_capabilities(manual), "manual".to_string());
    }

    let normalized = normalize_capabilities(discovered);
    if normalized.is_empty() {
        (Vec::new(), "unknown".to_string())
    } else {
        (normalized, "catalog".to_string())
    }
}

pub fn catalog_capabilities(model_id: &str) -> Vec<String> {
    let id = model_id.to_ascii_lowercase();

    if id.contains("transcrib") || id.contains("whisper") {
        return vec!["speech.asr".into()];
    }
    if id.contains("tts") || id.contains("text-to-speech") {
        return vec!["speech.tts".into()];
    }
    if id.contains("embed") {
        return vec!["embedding.text".into()];
    }
    if id.contains("image") || id.contains("dall-e") {
        return vec!["image.generate".into()];
    }

    if [
        "gpt",
        "qwen",
        "deepseek",
        "glm",
        "kimi",
        "claude",
        "gemini",
    ]
    .iter()
    .any(|prefix| id.contains(prefix))
    {
        return vec![
            "text.generate".into(),
            "text.translate".into(),
            "transcript.refine".into(),
        ];
    }

    Vec::new()
}

fn derive_model_list_url(provider: &ProviderConfig) -> Result<String, String> {
    let explicit = provider.model_list_url.trim();
    if !explicit.is_empty() {
        return Ok(explicit.to_string());
    }

    let base = provider.base_url.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err("provider model discovery URL is empty".into());
    }
    Ok(format!("{base}/models"))
}

fn model_array(value: &Value) -> Option<&Vec<Value>> {
    if let Some(data) = value.get("data").and_then(Value::as_array) {
        return Some(data);
    }
    if let Some(models) = value.get("models").and_then(Value::as_array) {
        return Some(models);
    }
    if let Some(models) = value
        .get("output")
        .and_then(|output| output.get("models"))
        .and_then(Value::as_array)
    {
        return Some(models);
    }
    if let Some(models) = value
        .get("result")
        .and_then(|result| result.get("models"))
        .and_then(Value::as_array)
    {
        return Some(models);
    }
    value.as_array()
}

fn string_field(value: &Value, names: &[&str]) -> Option<String> {
    for name in names {
        if let Some(value) = value.get(*name).and_then(Value::as_str) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn advertised_capabilities(value: &Value) -> Vec<String> {
    let mut values = Vec::new();

    for key in ["capabilities", "capability", "abilities"] {
        if let Some(array) = value.get(key).and_then(Value::as_array) {
            for item in array {
                if let Some(item) = item.as_str() {
                    values.push(item.to_string());
                }
            }
        }
    }

    normalize_capabilities(&values)
}

pub fn parse_model_response(value: &Value) -> Result<Vec<DiscoveredModel>, String> {
    let array = model_array(value).ok_or_else(|| "model list response contains no model array".to_string())?;
    let mut models = Vec::new();

    for item in array {
        let Some(model_id) = string_field(
            item,
            &["id", "model_id", "modelId", "model_name", "modelName", "name"],
        ) else {
            continue;
        };

        let display_name = string_field(item, &["display_name", "displayName", "name"])
            .unwrap_or_else(|| model_id.clone());
        let owned_by = string_field(item, &["owned_by", "ownedBy", "provider", "vendor"])
            .unwrap_or_default();

        let mut capabilities = advertised_capabilities(item);
        if capabilities.is_empty() {
            capabilities = catalog_capabilities(&model_id);
        }

        models.push(DiscoveredModel {
            model_id,
            display_name,
            owned_by,
            capabilities,
        });
    }

    models.sort_by(|a, b| a.model_id.cmp(&b.model_id));
    models.dedup_by(|a, b| a.model_id == b.model_id);

    if models.is_empty() {
        return Err("model discovery returned no identifiable model IDs".into());
    }

    Ok(models)
}

fn client() -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("Tsubame/0.1 ProviderRegistry")
        .build()
        .map_err(|e| e.to_string())
}

async fn model_request(
    provider: &ProviderConfig,
    secret: Option<&str>,
) -> Result<(StatusCode, Value), String> {
    let url = derive_model_list_url(provider)?;
    let mut request = client()?.get(url);

    match provider.auth_mode.as_str() {
        "none" => {}
        "bearer" => {
            let secret = secret.ok_or_else(|| "provider API key is not configured".to_string())?;
            request = request.bearer_auth(secret);
        }
        other => return Err(format!("unsupported provider auth mode: {other}")),
    }

    let response = request.send().await.map_err(|e| e.to_string())?;
    let status = response.status();
    let body = response.text().await.map_err(|e| e.to_string())?;

    if !status.is_success() {
        return Err(format!("provider returned HTTP {status}"));
    }

    let value = serde_json::from_str::<Value>(&body)
        .map_err(|e| format!("provider returned invalid JSON: {e}"))?;
    Ok((status, value))
}

pub async fn discover_models(
    provider: &ProviderConfig,
    secret: Option<&str>,
) -> Result<Vec<DiscoveredModel>, String> {
    let (_, value) = model_request(provider, secret).await?;
    parse_model_response(&value)
}

pub async fn test_provider(
    provider: &ProviderConfig,
    secret: Option<&str>,
) -> ProviderTestResult {
    match model_request(provider, secret).await {
        Ok((status, value)) => match parse_model_response(&value) {
            Ok(models) => ProviderTestResult {
                ok: true,
                status: status.as_u16().to_string(),
                model_count: models.len(),
                message: format!("Connected. {} models are visible to this credential.", models.len()),
            },
            Err(error) => ProviderTestResult {
                ok: false,
                status: status.as_u16().to_string(),
                model_count: 0,
                message: error,
            },
        },
        Err(error) => ProviderTestResult {
            ok: false,
            status: "error".into(),
            model_count: 0,
            message: error,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_openai_model_list() {
        let response = json!({
            "object": "list",
            "data": [
                {"id": "gpt-example", "owned_by": "example"},
                {"id": "whisper-example", "owned_by": "example"}
            ]
        });

        let models = parse_model_response(&response).unwrap();
        assert_eq!(models.len(), 2);
        assert!(models[0].capabilities.contains(&"text.generate".to_string()));
        assert!(models[1].capabilities.contains(&"speech.asr".to_string()));
    }

    #[test]
    fn parses_nested_model_studio_shape() {
        let response = json!({
            "output": {
                "models": [
                    {"model_name": "qwen-example", "provider": "Alibaba"}
                ]
            }
        });

        let models = parse_model_response(&response).unwrap();
        assert_eq!(models[0].model_id, "qwen-example");
        assert!(models[0].capabilities.contains(&"text.translate".to_string()));
    }

    #[test]
    fn manual_capabilities_replace_catalog_metadata() {
        let discovered = vec!["text.generate".into(), "text.translate".into()];
        let manual = vec!["speech.asr".into(), "not-a-capability".into()];
        let (resolved, source) = resolve_capabilities(&discovered, Some(&manual));
        assert_eq!(resolved, vec!["speech.asr"]);
        assert_eq!(source, "manual");
    }
}
