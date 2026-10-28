use crate::jobs::ProgressSink;
use crate::providers::ProviderConfig;
use reqwest::multipart::{Form, Part};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    env, fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Output, Stdio},
    time::Duration,
};

pub const ASR_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AsrSegmentCandidate {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AsrResult {
    pub contract_version: u32,
    pub adapter_id: String,
    pub model_id: String,
    pub language: Option<String>,
    pub segments: Vec<AsrSegmentCandidate>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
pub struct AsrRunInput {
    pub media_id: i64,
    pub provider_id: String,
    pub model_id: Option<String>,
    pub language: Option<String>,
    pub prompt: Option<String>,
}

/// Provider id retired by ADR-019 (docs/DECISIONS.md).
pub const RETIRED_WHISPER_CPP: &str = "local.whisper-cpp";
pub const MIGRATED_PROVIDER: &str = "local.faster-whisper";
pub const MIGRATED_MODEL: &str = "large-v3-turbo";

/// Input that still names the retired whisper.cpp provider (a retried job,
/// a CLI/MCP call, a saved selection) runs on local.faster-whisper +
/// large-v3-turbo. Returns the note the caller must put in the result notes,
/// so the switch is always visible; None leaves the input untouched.
pub fn migrate_retired_target(input: &mut AsrRunInput) -> Option<String> {
    if input.provider_id.trim() != RETIRED_WHISPER_CPP {
        return None;
    }
    let from_model = input.model_id.clone().unwrap_or_default();
    input.provider_id = MIGRATED_PROVIDER.into();
    input.model_id = Some(MIGRATED_MODEL.into());
    Some(format!(
        "migrated: {RETIRED_WHISPER_CPP} (model {}) is retired; ran {MIGRATED_PROVIDER} with {MIGRATED_MODEL}",
        if from_model.is_empty() { "default" } else { from_model.as_str() }
    ))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct WorkerRequest {
    contract_version: u32,
    adapter_id: String,
    media_path: String,
    media_duration_ms: i64,
    model_id: String,
    language: Option<String>,
    prompt: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct RemoteSegment {
    start: f64,
    end: f64,
    text: String,
    #[serde(default)]
    avg_logprob: Option<f64>,
}

fn worker_script(name: &str) -> Result<PathBuf, String> {
    // Debug builds run workers straight from the repository checkout.
    #[cfg(debug_assertions)]
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../workers/asr");
    // Release builds use the copy bundled via tauri.conf.json bundle.resources;
    // no build-machine path is compiled in.
    #[cfg(not(debug_assertions))]
    let dir = bundled_resource_dir()?.join("workers").join("asr");
    Ok(dir.join(name))
}

#[cfg(not(debug_assertions))]
fn bundled_resource_dir() -> Result<PathBuf, String> {
    let exe =
        env::current_exe().map_err(|e| format!("cannot locate the YUZUKI executable: {e}"))?;
    let exe_dir = exe
        .parent()
        .ok_or_else(|| "YUZUKI executable has no parent directory".to_string())?;
    if cfg!(target_os = "macos") {
        Ok(exe_dir.join("../Resources"))
    } else {
        Ok(exe_dir.to_path_buf())
    }
}

/// Error text a job cancel produces when it stops an ASR run.
pub const ASR_CANCELLED: &str = "transcription was cancelled";

/// Waits for `child` while draining its pipes, killing it as soon as `sink`
/// reports a cancel so Stop in the UI frees the CPU/GPU right away.
fn wait_cancellable(mut child: Child, sink: &dyn ProgressSink) -> Result<Output, String> {
    fn drain<R: Read + Send + 'static>(pipe: Option<R>) -> std::thread::JoinHandle<Vec<u8>> {
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut buf);
            }
            buf
        })
    }
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let status = loop {
        if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
            break status;
        }
        if sink.is_cancelled() {
            let _ = child.kill();
            let _ = child.wait();
            let _ = stdout.join();
            let _ = stderr.join();
            return Err(ASR_CANCELLED.into());
        }
        std::thread::sleep(Duration::from_millis(150));
    };
    Ok(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

/// Model caches for ASR workers, always under the data root so no download
/// lands in the per-user Hugging Face / ModelScope / torch cache on the
/// system drive. Inherited legacy cache variables are removed.
fn model_cache_envs() -> Vec<(&'static str, Option<PathBuf>)> {
    let Some(root) = crate::data_root::get() else {
        return Vec::new();
    };
    let models = root.join("models");
    let hf = models.join("huggingface");
    vec![
        ("YUZUKI_ASR_MODEL_DIR", Some(models.join("asr"))),
        ("HF_HOME", Some(hf.clone())),
        ("HF_HUB_CACHE", Some(hf.join("hub"))),
        ("HUGGINGFACE_HUB_CACHE", None),
        ("MODELSCOPE_CACHE", Some(models.join("modelscope"))),
        ("TORCH_HOME", Some(models.join("torch"))),
    ]
}

/// envs: Some sets the variable for the worker, None removes an inherited one.
fn run_json_worker(
    python: PathBuf,
    script_name: &str,
    request: &WorkerRequest,
    envs: &[(&str, Option<PathBuf>)],
    sink: &dyn ProgressSink,
) -> Result<AsrResult, String> {
    let script = worker_script(script_name)?;
    if !script.exists() {
        return Err(format!(
            "ASR worker script is missing: {}",
            script.display()
        ));
    }

    let mut command = Command::new(python);
    command
        .arg(script)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in envs {
        match value {
            Some(value) => command.env(name, value),
            None => command.env_remove(name),
        };
    }
    let mut child = command
        .spawn()
        .map_err(|e| format!("failed to start ASR worker: {e}"))?;

    let payload = serde_json::to_vec(request).map_err(|e| e.to_string())?;
    child
        .stdin
        .take()
        .ok_or_else(|| "ASR worker stdin unavailable".to_string())?
        .write_all(&payload)
        .map_err(|e| e.to_string())?;

    let output = wait_cancellable(child, sink)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ASR worker failed: {}", stderr.trim()));
    }

    let result: AsrResult = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("invalid ASR worker JSON: {e}"))?;
    validate_result(&result)?;
    Ok(result)
}

pub fn run_faster_whisper(
    media_path: &str,
    duration_ms: i64,
    model_id: &str,
    language: Option<String>,
    prompt: Option<String>,
    sink: &dyn ProgressSink,
) -> Result<AsrResult, String> {
    // The worker reads 16 kHz mono 16-bit PCM WAV with wave (no PyAV
    // decode); any other source is decoded into the decode cache first, and a
    // decode failure keeps its own code (no silent fallback).
    let input = crate::media_decode::ensure_wav(
        Path::new(media_path),
        crate::media_decode::DecodeTarget::ASR_16K_MONO,
        sink,
    )
    .map_err(|e| e.to_string())?;
    let media_path = input.path.to_string_lossy().into_owned();
    // The active GPU pack (if any) is handed over explicitly; without it the
    // worker runs on CPU or fails an explicit device=cuda request.
    let cuda_dir = crate::runtime::resolve_faster_whisper_cuda_dir();
    let mut envs = model_cache_envs();
    envs.push((crate::runtime::ENV_FW_CUDA_DIR, cuda_dir));
    run_json_worker(
        crate::runtime::resolve_faster_whisper_python()?,
        "faster_whisper_worker.py",
        &WorkerRequest {
            contract_version: ASR_CONTRACT_VERSION,
            adapter_id: "faster-whisper".into(),
            media_path,
            media_duration_ms: duration_ms,
            model_id: model_id.into(),
            language,
            prompt,
        },
        &envs,
        sink,
    )
}

pub fn run_funasr(
    media_path: &str,
    duration_ms: i64,
    model_id: &str,
    language: Option<String>,
    prompt: Option<String>,
    sink: &dyn ProgressSink,
) -> Result<AsrResult, String> {
    // Same 16 kHz mono WAV as faster-whisper: the worker reads it with
    // soundfile and splits it with FSMN-VAD.
    let input = crate::media_decode::ensure_wav(
        Path::new(media_path),
        crate::media_decode::DecodeTarget::ASR_16K_MONO,
        sink,
    )
    .map_err(|e| e.to_string())?;
    let media_path = input.path.to_string_lossy().into_owned();
    run_json_worker(
        crate::runtime::resolve_funasr_python()?,
        "funasr_worker.py",
        &WorkerRequest {
            contract_version: ASR_CONTRACT_VERSION,
            adapter_id: "funasr-sensevoice".into(),
            media_path,
            media_duration_ms: duration_ms,
            model_id: model_id.into(),
            language,
            prompt,
        },
        &model_cache_envs(),
        sink,
    )
}

pub fn run_apple_speech(
    media_path: &str,
    duration_ms: i64,
    language: Option<String>,
    sink: &dyn ProgressSink,
) -> Result<AsrResult, String> {
    if !cfg!(target_os = "macos") {
        return Err("Apple SpeechAnalyzer is only available on supported Apple platforms".into());
    }

    let helper = env::var("TSUBAME_APPLE_SPEECH_HELPER")
        .map_err(|_| "TSUBAME_APPLE_SPEECH_HELPER is not configured".to_string())?;
    let request = WorkerRequest {
        contract_version: ASR_CONTRACT_VERSION,
        adapter_id: "apple-speech".into(),
        media_path: media_path.into(),
        media_duration_ms: duration_ms,
        model_id: "speech-transcriber".into(),
        language,
        prompt: None,
    };

    let mut child = Command::new(helper)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to start Apple Speech helper: {e}"))?;
    child
        .stdin
        .take()
        .ok_or_else(|| "Apple Speech helper stdin unavailable".to_string())?
        .write_all(&serde_json::to_vec(&request).map_err(|e| e.to_string())?)
        .map_err(|e| e.to_string())?;
    let output = wait_cancellable(child, sink)?;
    if !output.status.success() {
        return Err(format!(
            "Apple Speech helper failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    let result: AsrResult = serde_json::from_slice(&output.stdout)
        .map_err(|e| format!("invalid Apple Speech JSON: {e}"))?;
    validate_result(&result)?;
    Ok(result)
}

#[allow(clippy::too_many_arguments)]
async fn post_remote_transcription(
    provider: &ProviderConfig,
    secret: Option<&str>,
    url: &str,
    media_bytes: &[u8],
    filename: &str,
    model_id: &str,
    language: Option<&str>,
    prompt: Option<&str>,
    verbose: bool,
) -> Result<(reqwest::StatusCode, Vec<u8>), String> {
    let mut form = Form::new()
        .part(
            "file",
            Part::bytes(media_bytes.to_vec()).file_name(filename.to_string()),
        )
        .text("model", model_id.to_string());

    if verbose {
        form = form
            .text("response_format", "verbose_json")
            .text("timestamp_granularities[]", "segment");
    } else {
        form = form.text("response_format", "json");
    }

    if let Some(language) = language.filter(|value| !value.is_empty() && *value != "auto") {
        form = form.text("language", language.to_string());
    }
    if let Some(prompt) = prompt.filter(|value| !value.trim().is_empty()) {
        form = form.text("prompt", prompt.to_string());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(300))
        .user_agent(concat!("YUZUKI/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| e.to_string())?;
    let mut request = client.post(url).multipart(form);

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
    let body = response.bytes().await.map_err(|e| e.to_string())?.to_vec();
    Ok((status, body))
}

pub async fn run_remote_openai_compatible(
    provider: &ProviderConfig,
    secret: Option<&str>,
    media_path: &str,
    media_duration_ms: i64,
    model_id: &str,
    language: Option<String>,
    prompt: Option<String>,
) -> Result<AsrResult, String> {
    let media_bytes = fs::read(media_path).map_err(|e| format!("cannot read media: {e}"))?;
    let filename = Path::new(media_path)
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("audio.bin")
        .to_string();
    let url = format!(
        "{}/audio/transcriptions",
        provider.base_url.trim_end_matches('/')
    );

    let (status, body) = post_remote_transcription(
        provider,
        secret,
        &url,
        &media_bytes,
        &filename,
        model_id,
        language.as_deref(),
        prompt.as_deref(),
        true,
    )
    .await?;

    let (status, body, notes) = if status.is_success() {
        (status, body, Vec::new())
    } else if matches!(
        status,
        reqwest::StatusCode::BAD_REQUEST | reqwest::StatusCode::UNPROCESSABLE_ENTITY
    ) {
        // Some OpenAI-compatible transcription models expose only plain JSON.
        // Negotiate down without branching on vendor/model names.
        let (fallback_status, fallback_body) = post_remote_transcription(
            provider,
            secret,
            &url,
            &media_bytes,
            &filename,
            model_id,
            language.as_deref(),
            prompt.as_deref(),
            false,
        )
        .await?;
        (
            fallback_status,
            fallback_body,
            vec!["Provider rejected timestamped verbose_json; retried plain JSON.".to_string()],
        )
    } else {
        return Err(format!("ASR provider returned HTTP {status}"));
    };

    if !status.is_success() {
        return Err(format!("ASR provider returned HTTP {status}"));
    }

    let value: Value = serde_json::from_slice(&body)
        .map_err(|e| format!("ASR provider returned invalid JSON: {e}"))?;
    let mut result = parse_remote_response(model_id, media_duration_ms, &value)?;
    result.notes.extend(notes);
    Ok(result)
}

pub fn parse_remote_response(
    model_id: &str,
    media_duration_ms: i64,
    value: &Value,
) -> Result<AsrResult, String> {
    let language = value
        .get("language")
        .and_then(Value::as_str)
        .map(str::to_string);

    let mut segments = Vec::new();
    if let Some(rows) = value.get("segments").and_then(Value::as_array) {
        for row in rows {
            let parsed: RemoteSegment = serde_json::from_value(row.clone())
                .map_err(|e| format!("invalid ASR segment: {e}"))?;
            segments.push(AsrSegmentCandidate {
                start_ms: (parsed.start * 1000.0).round() as i64,
                end_ms: (parsed.end * 1000.0).round() as i64,
                text: parsed.text.trim().to_string(),
                confidence: parsed.avg_logprob.map(|score| score.exp().clamp(0.0, 1.0)),
            });
        }
    }

    if segments.is_empty() {
        let text = value
            .get("text")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| "ASR response contains neither segments nor text".to_string())?;

        if media_duration_ms <= 0 {
            return Err(
                "ASR returned text without timestamps and media duration is unknown".into(),
            );
        }
        segments.push(AsrSegmentCandidate {
            start_ms: 0,
            end_ms: media_duration_ms,
            text: text.to_string(),
            confidence: None,
        });
    }

    let result = AsrResult {
        contract_version: ASR_CONTRACT_VERSION,
        adapter_id: "remote-openai-compatible".into(),
        model_id: model_id.into(),
        language,
        segments,
        notes: vec![],
    };
    validate_result(&result)?;
    Ok(result)
}

pub fn validate_result(result: &AsrResult) -> Result<(), String> {
    if result.contract_version != ASR_CONTRACT_VERSION {
        return Err(format!(
            "unsupported ASR contract version: {}",
            result.contract_version
        ));
    }
    if result.segments.is_empty() {
        return Err("ASR produced no segments".into());
    }

    let mut previous_start = -1_i64;
    for (index, segment) in result.segments.iter().enumerate() {
        if segment.start_ms < 0 || segment.end_ms <= segment.start_ms {
            return Err(format!("invalid ASR timing at segment {index}"));
        }
        if segment.start_ms < previous_start {
            return Err(format!("ASR segments are not ordered at segment {index}"));
        }
        if segment.text.trim().is_empty() {
            return Err(format!("ASR segment {index} is empty"));
        }
        previous_start = segment.start_ms;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run_input(provider: &str, model: Option<&str>) -> AsrRunInput {
        AsrRunInput {
            media_id: 1,
            provider_id: provider.into(),
            model_id: model.map(Into::into),
            language: Some("ja".into()),
            prompt: None,
        }
    }

    #[test]
    fn retired_whisper_cpp_input_migrates_with_a_note() {
        let mut input = run_input("local.whisper-cpp", Some("small"));
        let note = migrate_retired_target(&mut input).expect("migration note");
        assert_eq!(input.provider_id, "local.faster-whisper");
        assert_eq!(input.model_id.as_deref(), Some("large-v3-turbo"));
        assert_eq!(input.language.as_deref(), Some("ja"));
        assert!(note.contains("local.whisper-cpp (model small)"), "{note}");
        assert!(note.contains("large-v3-turbo"), "{note}");

        let mut default_model = run_input(" local.whisper-cpp ", None);
        let note = migrate_retired_target(&mut default_model).expect("migration note");
        assert!(note.contains("(model default)"), "{note}");
    }

    #[test]
    fn other_providers_are_not_migrated() {
        for provider in ["local.faster-whisper", "local.funasr", "remote.openai"] {
            let mut input = run_input(provider, Some("m"));
            assert!(migrate_retired_target(&mut input).is_none());
            assert_eq!(input.provider_id, provider);
            assert_eq!(input.model_id.as_deref(), Some("m"));
        }
    }

    #[test]
    fn parses_verbose_remote_segments() {
        let value = json!({
            "language": "ja",
            "segments": [
                {"start": 0.25, "end": 1.5, "text": " おはよう", "avg_logprob": -0.2},
                {"start": 1.5, "end": 2.8, "text": "ございます"}
            ]
        });
        let result = parse_remote_response("whisper-1", 3000, &value).unwrap();
        assert_eq!(result.language.as_deref(), Some("ja"));
        assert_eq!(result.segments[0].start_ms, 250);
        assert_eq!(result.segments[1].text, "ございます");
    }

    #[test]
    fn plain_json_falls_back_to_whole_track() {
        let value = json!({"text":"こんばんは"});
        let result = parse_remote_response("example-asr", 4200, &value).unwrap();
        assert_eq!(result.segments.len(), 1);
        assert_eq!(result.segments[0].end_ms, 4200);
    }

    #[test]
    fn japanese_smoke_fixture_is_valid() {
        let fixture = include_str!("../../../fixtures/asr/ja-smoke.json");
        let result: AsrResult = serde_json::from_str(fixture).unwrap();
        validate_result(&result).unwrap();
        assert_eq!(result.language.as_deref(), Some("ja"));
        assert!(result
            .segments
            .iter()
            .any(|segment| segment.text.contains("おやすみ")));
    }
}

#[cfg(test)]
mod cancel_tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Instant;

    struct FlagSink(AtomicBool);
    impl ProgressSink for FlagSink {
        fn progress(&self, _stage: &str, _done: u64, _total: u64) {}
        fn is_cancelled(&self) -> bool {
            self.0.load(Ordering::SeqCst)
        }
    }

    fn long_running() -> Child {
        let mut cmd = if cfg!(windows) {
            let mut c = Command::new("ping");
            c.args(["-n", "30", "127.0.0.1"]);
            c
        } else {
            let mut c = Command::new("sleep");
            c.arg("30");
            c
        };
        cmd.stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    }

    #[test]
    fn cancel_kills_asr_subprocess_promptly() {
        let sink = FlagSink(AtomicBool::new(true));
        let started = Instant::now();
        let err = wait_cancellable(long_running(), &sink).unwrap_err();
        assert_eq!(err, ASR_CANCELLED);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn uncancelled_subprocess_output_is_collected() {
        let sink = FlagSink(AtomicBool::new(false));
        let child = if cfg!(windows) {
            Command::new("cmd")
                .args(["/C", "echo yuzuki"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        } else {
            Command::new("sh")
                .args(["-c", "echo yuzuki"])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
                .unwrap()
        };
        let output = wait_cancellable(child, &sink).unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("yuzuki"));
    }
}
