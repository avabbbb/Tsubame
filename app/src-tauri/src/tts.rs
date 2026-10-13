//! Speech synthesis (`speech.tts`) for the dubbing pipeline.
//!
//! Rust owns the whole single-sentence path: it resolves the Voice Profile and
//! Provider, computes a content-addressed cache key, asks an adapter for audio
//! only on a cache miss, validates and normalizes the WAV, stores it
//! atomically under `<app data>/assets/tts/<key[0..2]>/<key>.wav`, records it
//! in `tts_assets` and finally points the Segment at it with a
//! revision-checked write.
//!
//! Contract: docs/19-dubbing-pipeline.md § "speech.tts and voices".
//!
//! Reference lineage: the cache-key-per-synthesis-input idea and the
//! OpenAI-compatible `/audio/speech` request shape follow ASMR-Dubber
//! `tts.py:tts_cache_key` / `tts_backends.py:_generic_tts_api_runner`
//! (MIT, Copyright (c) 2026 ASMR Dubber contributors,
//! https://github.com/EveningStudy/asmr-dubber, commit `2faea94f`). This is a
//! re-implementation, evolved for Tsubame: the key excludes sentence id,
//! source-media hash and timing (timing edits never invalidate TTS), adapters
//! are chosen by Provider kind (never by model name), and there is no
//! reference-audio / voice-clone path.

use crate::db::{MediaDb, Segment};
use crate::providers::ProviderConfig;
use crate::voice::{self, hex, VoiceProfile};
use reqwest::Client;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::fmt;
use std::future::Future;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;
use unicode_normalization::UnicodeNormalization;

pub const TTS_CONTRACT_VERSION: u32 = 1;
/// Domain separator of the cache key. Bump only if the key layout changes.
const CACHE_KEY_DOMAIN: &str = "tsubame.speech.tts.key.v1";
pub const OPENAI_COMPATIBLE_ADAPTER_VERSION: &str = "openai-compatible/audio-speech@1";
pub const MOCK_ADAPTER_VERSION: &str = "mock@1";

/// Request fields the adapter owns. Voice-profile params cannot override them.
const RESERVED_REQUEST_FIELDS: &[&str] =
    &["model", "input", "voice", "instructions", "response_format"];

// ----- errors / progress -----

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TtsError {
    /// Stable slug: `revision_conflict`, `not_found`, `invalid_input`,
    /// `provider_unavailable`, `http`, `invalid_audio`, `io`, `cancelled`, ...
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl TtsError {
    pub fn new(code: &str, message: impl Into<String>, retryable: bool) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable,
        }
    }
    fn input(message: impl Into<String>) -> Self {
        Self::new("invalid_input", message, false)
    }
    fn db(error: impl fmt::Display) -> Self {
        Self::new("db", error.to_string(), true)
    }
    fn io(error: impl fmt::Display) -> Self {
        Self::new("io", error.to_string(), true)
    }
}

impl fmt::Display for TtsError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.code == "revision_conflict" {
            // The Inspector matches on this phrase for its conflict UI.
            write!(f, "segment revision conflict: {}", self.message)
        } else {
            write!(f, "{}: {}", self.code, self.message)
        }
    }
}

/// Minimal progress/cancellation hook. Kept local so this module does not
/// depend on the job runner; the integrator adapts a job handle to it.
pub trait ProgressSink: Send + Sync {
    fn progress(&self, stage: &str, done: u64, total: u64);
    fn is_cancelled(&self) -> bool;
}

pub struct NoopProgress;

impl ProgressSink for NoopProgress {
    fn progress(&self, _stage: &str, _done: u64, _total: u64) {}
    fn is_cancelled(&self) -> bool {
        false
    }
}

fn check_cancelled(sink: &dyn ProgressSink) -> Result<(), TtsError> {
    if sink.is_cancelled() {
        Err(TtsError::new("cancelled", "synthesis was cancelled", true))
    } else {
        Ok(())
    }
}

// ----- cache key -----

/// NFC, trimmed, internal whitespace runs collapsed to one ASCII space.
pub fn normalize_text(text: &str) -> String {
    let nfc: String = text.nfc().collect();
    nfc.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[derive(Debug, Clone, PartialEq)]
pub struct CacheKeyInput<'a> {
    pub provider_id: &'a str,
    pub model_id: &'a str,
    pub adapter_version: &'a str,
    /// Already normalized with [`normalize_text`].
    pub normalized_text: &'a str,
    pub language: &'a str,
    pub voice_profile_hash: &'a str,
    /// Canonical JSON (see [`voice::canonical_json`]).
    pub params_json: &'a str,
    pub take: u32,
}

/// `sha256(provider_id ‖ model_id ‖ adapter_version ‖ normalized_text ‖
/// language ‖ voice_profile_hash ‖ params_json ‖ take)`. Every field is
/// length-prefixed so concatenation is unambiguous. Segment id, source media
/// and timing are intentionally not inputs.
pub fn cache_key(input: &CacheKeyInput<'_>) -> String {
    let take = input.take.to_string();
    let mut hasher = Sha256::new();
    hasher.update(CACHE_KEY_DOMAIN.as_bytes());
    for field in [
        input.provider_id,
        input.model_id,
        input.adapter_version,
        input.normalized_text,
        input.language,
        input.voice_profile_hash,
        input.params_json,
        take.as_str(),
    ] {
        hasher.update((field.len() as u64).to_be_bytes());
        hasher.update(field.as_bytes());
    }
    hex(&hasher.finalize())
}

// ----- adapter -----

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TtsRequest {
    pub contract_version: u32,
    pub model_id: String,
    pub text: String,
    pub language: String,
    /// `preset` or `design`.
    pub voice_kind: String,
    pub preset_voice: String,
    pub design_prompt: String,
    /// Canonical JSON object from the Voice Profile.
    pub params: Value,
    pub take: u32,
}

/// One speech.tts transport. Implementations return raw WAV bytes; the
/// caller validates and stores them.
pub trait TtsAdapter: Send + Sync {
    /// Part of the cache key: bump when the adapter's output for the same
    /// inputs may change.
    fn adapter_version(&self) -> &'static str;

    fn synthesize<'a>(
        &'a self,
        request: &'a TtsRequest,
        sink: &'a dyn ProgressSink,
    ) -> impl Future<Output = Result<Vec<u8>, TtsError>> + Send + 'a;
}

/// OpenAI-compatible `POST {base_url}/audio/speech`.
pub struct OpenAiCompatibleTts {
    pub base_url: String,
    pub auth_mode: String,
    pub secret: Option<String>,
    pub timeout: Duration,
}

impl OpenAiCompatibleTts {
    pub fn from_provider(provider: &ProviderConfig, secret: Option<String>) -> Self {
        Self {
            base_url: provider.base_url.trim().trim_end_matches('/').to_string(),
            auth_mode: provider.auth_mode.clone(),
            secret,
            timeout: Duration::from_secs(120),
        }
    }

    /// Builds the JSON body. `instructions` is sent only for design voices.
    /// Profile params are merged at top level (e.g. `speed`) but cannot
    /// override adapter-owned fields.
    pub fn request_body(request: &TtsRequest) -> Value {
        let mut body = Map::new();
        if let Value::Object(params) = &request.params {
            for (key, value) in params {
                if !RESERVED_REQUEST_FIELDS.contains(&key.as_str()) {
                    body.insert(key.clone(), value.clone());
                }
            }
        }
        body.insert("model".into(), Value::String(request.model_id.clone()));
        body.insert("input".into(), Value::String(request.text.clone()));
        if !request.preset_voice.trim().is_empty() {
            body.insert(
                "voice".into(),
                Value::String(request.preset_voice.trim().to_string()),
            );
        }
        if request.voice_kind == "design" && !request.design_prompt.trim().is_empty() {
            body.insert(
                "instructions".into(),
                Value::String(request.design_prompt.trim().to_string()),
            );
        }
        body.insert("response_format".into(), Value::String("wav".into()));
        Value::Object(body)
    }
}

impl TtsAdapter for OpenAiCompatibleTts {
    fn adapter_version(&self) -> &'static str {
        OPENAI_COMPATIBLE_ADAPTER_VERSION
    }

    fn synthesize<'a>(
        &'a self,
        request: &'a TtsRequest,
        sink: &'a dyn ProgressSink,
    ) -> impl Future<Output = Result<Vec<u8>, TtsError>> + Send + 'a {
        async move {
            if self.base_url.is_empty() {
                return Err(TtsError::new(
                    "provider_unavailable",
                    "provider inference base URL is empty",
                    false,
                ));
            }
            check_cancelled(sink)?;
            sink.progress("synthesize", 0, 1);
            let mut builder = Client::builder()
                .timeout(self.timeout)
                .user_agent("Tsubame/0.1 SpeechTts");
            if is_loopback_url(&self.base_url) {
                // A localhost server must never be routed through a system proxy.
                builder = builder.no_proxy();
            }
            let client = builder
                .build()
                .map_err(|e| TtsError::new("http", e.to_string(), false))?;
            let mut http = client
                .post(format!("{}/audio/speech", self.base_url))
                .json(&Self::request_body(request));
            match self.auth_mode.as_str() {
                "none" => {}
                "bearer" => {
                    let secret = self.secret.as_deref().ok_or_else(|| {
                        TtsError::new(
                            "provider_unavailable",
                            "provider API key is not configured",
                            false,
                        )
                    })?;
                    http = http.bearer_auth(secret);
                }
                other => {
                    return Err(TtsError::new(
                        "provider_unavailable",
                        format!("unsupported provider auth mode: {other}"),
                        false,
                    ))
                }
            }
            let response = http
                .send()
                .await
                .map_err(|e| TtsError::new("http", e.without_url().to_string(), true))?;
            let status = response.status();
            let bytes = response
                .bytes()
                .await
                .map_err(|e| TtsError::new("http", e.without_url().to_string(), true))?;
            check_cancelled(sink)?;
            if !status.is_success() {
                let excerpt: String = String::from_utf8_lossy(&bytes).chars().take(300).collect();
                let retryable = status.as_u16() == 429 || status.is_server_error();
                return Err(TtsError::new(
                    "http",
                    format!("tts provider returned HTTP {status}: {excerpt}"),
                    retryable,
                ));
            }
            sink.progress("synthesize", 1, 1);
            Ok(bytes.to_vec())
        }
    }
}

/// True for `http(s)://localhost|127.x|[::1]` base URLs.
pub fn is_loopback_url(url: &str) -> bool {
    let Ok(parsed) = reqwest::Url::parse(url) else {
        return false;
    };
    let Some(host) = parsed.host_str() else {
        return false;
    };
    let host = host.trim_start_matches('[').trim_end_matches(']');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

/// Deterministic offline adapter for tests (and integrator smoke tests).
/// Emits a mono 16-bit sine whose length grows with the text length.
#[cfg_attr(not(test), allow(dead_code))]
pub struct MockTts {
    pub sample_rate: u32,
    pub calls: std::sync::atomic::AtomicUsize,
    pub last_request: Mutex<Option<TtsRequest>>,
}

#[cfg_attr(not(test), allow(dead_code))]
impl MockTts {
    pub fn new() -> Self {
        Self {
            sample_rate: 24_000,
            calls: std::sync::atomic::AtomicUsize::new(0),
            last_request: Mutex::new(None),
        }
    }
    pub fn call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

impl TtsAdapter for MockTts {
    fn adapter_version(&self) -> &'static str {
        MOCK_ADAPTER_VERSION
    }

    fn synthesize<'a>(
        &'a self,
        request: &'a TtsRequest,
        sink: &'a dyn ProgressSink,
    ) -> impl Future<Output = Result<Vec<u8>, TtsError>> + Send + 'a {
        async move {
            check_cancelled(sink)?;
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if let Ok(mut last) = self.last_request.lock() {
                *last = Some(request.clone());
            }
            let chars = request.text.chars().count().max(1) as u32;
            let frames = self.sample_rate / 10 * chars.min(200); // 100 ms per char
            let samples = (0..frames)
                .map(|i| {
                    let t = i as f32 / self.sample_rate as f32;
                    ((t * 440.0 * std::f32::consts::TAU).sin() * 8_000.0) as i16
                })
                .collect::<Vec<_>>();
            Ok(encode_pcm16_wav(self.sample_rate, 1, &samples))
        }
    }
}

/// Adapters are chosen by Provider **kind**, never by model id.
pub enum AnyTtsAdapter {
    OpenAiCompatible(OpenAiCompatibleTts),
    /// Never selected from a Provider kind; constructed directly by tests and
    /// integrator smoke checks.
    #[allow(dead_code)]
    Mock(MockTts),
}

pub fn adapter_for_provider(
    provider: &ProviderConfig,
    secret: Option<String>,
) -> Result<AnyTtsAdapter, TtsError> {
    match provider.kind.as_str() {
        "openai-compatible" => Ok(AnyTtsAdapter::OpenAiCompatible(
            OpenAiCompatibleTts::from_provider(provider, secret),
        )),
        other => Err(TtsError::new(
            "provider_unavailable",
            format!("provider adapter {other} does not implement speech.tts yet"),
            false,
        )),
    }
}

impl TtsAdapter for AnyTtsAdapter {
    fn adapter_version(&self) -> &'static str {
        match self {
            Self::OpenAiCompatible(adapter) => adapter.adapter_version(),
            Self::Mock(adapter) => adapter.adapter_version(),
        }
    }

    fn synthesize<'a>(
        &'a self,
        request: &'a TtsRequest,
        sink: &'a dyn ProgressSink,
    ) -> impl Future<Output = Result<Vec<u8>, TtsError>> + Send + 'a {
        async move {
            match self {
                Self::OpenAiCompatible(adapter) => adapter.synthesize(request, sink).await,
                Self::Mock(adapter) => adapter.synthesize(request, sink).await,
            }
        }
    }
}

// ----- WAV -----

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
pub struct WavInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    /// 1 = PCM, 3 = IEEE float, 0xFFFE = extensible.
    pub format_tag: u16,
    pub frames: u64,
    pub duration_ms: i64,
}

fn le_u16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([bytes[at], bytes[at + 1]])
}

fn le_u32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([bytes[at], bytes[at + 1], bytes[at + 2], bytes[at + 3]])
}

fn bad_wav(message: &str) -> TtsError {
    TtsError::new("invalid_audio", message, false)
}

/// Parses a RIFF/WAVE buffer and returns `(info, fmt_chunk_body, pcm_data)`.
/// Tolerates streaming headers whose RIFF/data sizes are 0 or 0xFFFFFFFF by
/// clamping the data chunk to the bytes actually present.
fn parse_wav_parts(bytes: &[u8]) -> Result<(WavInfo, &[u8], &[u8]), TtsError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(bad_wav("audio is not a RIFF/WAVE file"));
    }
    let mut at = 12usize;
    let mut fmt: Option<&[u8]> = None;
    let mut data: Option<&[u8]> = None;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let declared = le_u32(bytes, at + 4) as usize;
        let body_start = at + 8;
        let available = bytes.len() - body_start;
        if id == b"data" {
            let size = if declared == 0 || declared > available {
                available
            } else {
                declared
            };
            data = Some(&bytes[body_start..body_start + size]);
            break;
        }
        if declared > available {
            return Err(bad_wav("WAV chunk is truncated"));
        }
        if id == b"fmt " {
            fmt = Some(&bytes[body_start..body_start + declared]);
        }
        at = body_start + declared + (declared & 1);
    }
    let fmt = fmt.ok_or_else(|| bad_wav("WAV has no fmt chunk"))?;
    let data = data.ok_or_else(|| bad_wav("WAV has no data chunk"))?;
    if fmt.len() < 16 {
        return Err(bad_wav("WAV fmt chunk is too short"));
    }
    let format_tag = le_u16(fmt, 0);
    let channels = le_u16(fmt, 2);
    let sample_rate = le_u32(fmt, 4);
    let block_align = le_u16(fmt, 12);
    let bits_per_sample = le_u16(fmt, 14);
    if !matches!(format_tag, 1 | 3 | 0xFFFE) {
        return Err(bad_wav("WAV encoding is not PCM or float"));
    }
    if channels == 0 || sample_rate == 0 || block_align == 0 || bits_per_sample == 0 {
        return Err(bad_wav("WAV header has zero channels/rate/block size"));
    }
    if block_align as u32 != channels as u32 * bits_per_sample.div_ceil(8) as u32 {
        return Err(bad_wav(
            "WAV block alignment does not match channels/bit depth",
        ));
    }
    let frames = (data.len() / block_align as usize) as u64;
    let data = &data[..frames as usize * block_align as usize];
    let duration_ms = (frames * 1000 / sample_rate as u64) as i64;
    if duration_ms <= 0 {
        return Err(bad_wav("WAV contains no audio"));
    }
    Ok((
        WavInfo {
            sample_rate,
            channels,
            bits_per_sample,
            format_tag,
            frames,
            duration_ms,
        },
        fmt,
        data,
    ))
}

/// Header-only probe; kept public for placement/mix callers.
#[cfg_attr(not(test), allow(dead_code))]
pub fn parse_wav(bytes: &[u8]) -> Result<WavInfo, TtsError> {
    parse_wav_parts(bytes).map(|(info, _, _)| info)
}

/// Rewrites a WAV as `RIFF + fmt + data` with exact sizes, dropping other
/// chunks (LIST, etc.). Downstream mix/placement can then trust the header.
pub fn normalize_wav(bytes: &[u8]) -> Result<(Vec<u8>, WavInfo), TtsError> {
    let (info, fmt, data) = parse_wav_parts(bytes)?;
    let fmt_padded = fmt.len() + (fmt.len() & 1);
    let riff_size = 4 + 8 + fmt_padded + 8 + data.len() + (data.len() & 1);
    let mut out = Vec::with_capacity(8 + riff_size);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(riff_size as u32).to_le_bytes());
    out.extend_from_slice(b"WAVE");
    out.extend_from_slice(b"fmt ");
    out.extend_from_slice(&(fmt.len() as u32).to_le_bytes());
    out.extend_from_slice(fmt);
    if fmt.len() & 1 == 1 {
        out.push(0);
    }
    out.extend_from_slice(b"data");
    out.extend_from_slice(&(data.len() as u32).to_le_bytes());
    out.extend_from_slice(data);
    if data.len() & 1 == 1 {
        out.push(0);
    }
    Ok((out, info))
}

pub fn encode_pcm16_wav(sample_rate: u32, channels: u16, samples: &[i16]) -> Vec<u8> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&channels.to_le_bytes());
    out.extend_from_slice(&sample_rate.to_le_bytes());
    out.extend_from_slice(&(sample_rate * channels as u32 * 2).to_le_bytes());
    out.extend_from_slice(&(channels * 2).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for sample in samples {
        out.extend_from_slice(&sample.to_le_bytes());
    }
    out
}

// ----- asset store -----

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

/// Content-addressed TTS asset store rooted at `<app data>/assets`.
#[derive(Debug, Clone)]
pub struct AssetStore {
    pub root: PathBuf,
}

impl AssetStore {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// Store-relative path, `tts/<key[0..2]>/<key>.wav` (always `/`).
    pub fn relative_path(key: &str) -> Result<String, TtsError> {
        if key.len() != 64 || !key.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(TtsError::input("invalid TTS asset key"));
        }
        Ok(format!("tts/{}/{}.wav", &key[..2], key))
    }

    /// Resolves a stored relative path. Rejects anything that could escape
    /// the store.
    pub fn absolute(&self, relative: &str) -> Result<PathBuf, TtsError> {
        let rel = Path::new(relative);
        if rel.is_absolute()
            || rel
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return Err(TtsError::new(
                "invalid_asset",
                "asset path escapes the store",
                false,
            ));
        }
        Ok(self.root.join(rel))
    }

    /// Writes `bytes` atomically: temp file in the same directory, fsync,
    /// rename over the final path. A crash never leaves a partial `.wav`.
    pub fn write_atomic(&self, relative: &str, bytes: &[u8]) -> Result<PathBuf, TtsError> {
        let target = self.absolute(relative)?;
        let dir = target
            .parent()
            .ok_or_else(|| TtsError::io("asset path has no parent"))?;
        std::fs::create_dir_all(dir).map_err(TtsError::io)?;
        let file_name = target
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("asset");
        let tmp = dir.join(format!(".{file_name}.tmp-{}", uuid::Uuid::new_v4()));
        let result = (|| -> std::io::Result<()> {
            let mut file = std::fs::File::create(&tmp)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            std::fs::rename(&tmp, &target)
        })();
        if let Err(error) = result {
            let _ = std::fs::remove_file(&tmp);
            return Err(TtsError::io(error));
        }
        Ok(target)
    }
}

// ----- persistence -----

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TtsAsset {
    pub key: String,
    /// Relative to the asset store root (`tts/ab/<key>.wav`).
    pub path: String,
    pub sha256: String,
    pub duration_ms: i64,
    pub sample_rate: i64,
    pub channels: i64,
    pub provider_id: String,
    pub model_id: String,
    pub voice_profile_id: String,
    pub created_at: String,
}

/// A Segment's TTS clip resolved to an absolute file. This is what placement
/// (`dub.place` needs `duration_ms`) and mix (`audio.mix` needs `path`) use.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ResolvedTtsAsset {
    pub segment_id: i64,
    pub key: String,
    pub path: PathBuf,
    pub sha256: String,
    pub duration_ms: i64,
    pub sample_rate: i64,
    pub channels: i64,
    pub provider_id: String,
    pub model_id: String,
    pub voice_profile_id: String,
    pub dub_skip: bool,
}

fn ensure_column(
    conn: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let exists = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?
        .iter()
        .any(|name| name == column);
    if !exists {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN {column} {definition};"
        ))?;
    }
    Ok(())
}

/// Idempotent migration for `voice_profiles`, `tts_assets` and the
/// `segments.tts_asset_key` / `segments.dub_skip` columns. Safe on databases
/// created before PR #10. Called from `MediaDb::open` after `segments` exists.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    voice::migrate(conn)?;
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS tts_assets (
           key TEXT PRIMARY KEY,
           path TEXT NOT NULL,
           sha256 TEXT NOT NULL,
           duration_ms INTEGER NOT NULL,
           sample_rate INTEGER NOT NULL,
           channels INTEGER NOT NULL,
           provider_id TEXT NOT NULL,
           model_id TEXT NOT NULL,
           voice_profile_id TEXT NOT NULL DEFAULT '',
           created_at TEXT NOT NULL DEFAULT (datetime('now'))
         );",
    )?;
    ensure_column(conn, "segments", "tts_asset_key", "TEXT")?;
    ensure_column(conn, "segments", "dub_skip", "INTEGER NOT NULL DEFAULT 0")?;
    Ok(())
}

pub fn get_asset(conn: &Connection, key: &str) -> rusqlite::Result<Option<TtsAsset>> {
    conn.query_row(
        "SELECT key,path,sha256,duration_ms,sample_rate,channels,provider_id,model_id,
                voice_profile_id,created_at
         FROM tts_assets WHERE key=?1",
        [key],
        |row| {
            Ok(TtsAsset {
                key: row.get(0)?,
                path: row.get(1)?,
                sha256: row.get(2)?,
                duration_ms: row.get(3)?,
                sample_rate: row.get(4)?,
                channels: row.get(5)?,
                provider_id: row.get(6)?,
                model_id: row.get(7)?,
                voice_profile_id: row.get(8)?,
                created_at: row.get(9)?,
            })
        },
    )
    .optional()
}

fn upsert_asset(conn: &Connection, asset: &TtsAsset) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO tts_assets(
            key,path,sha256,duration_ms,sample_rate,channels,provider_id,model_id,voice_profile_id
         ) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)
         ON CONFLICT(key) DO UPDATE SET
            path=excluded.path,sha256=excluded.sha256,duration_ms=excluded.duration_ms,
            sample_rate=excluded.sample_rate,channels=excluded.channels",
        params![
            asset.key,
            asset.path,
            asset.sha256,
            asset.duration_ms,
            asset.sample_rate,
            asset.channels,
            asset.provider_id,
            asset.model_id,
            asset.voice_profile_id
        ],
    )?;
    Ok(())
}

/// `(tts_asset_key, dub_skip)` for a Segment, or `None` if it does not exist.
pub fn segment_dub_fields(
    conn: &Connection,
    segment_id: i64,
) -> rusqlite::Result<Option<(Option<String>, bool)>> {
    conn.query_row(
        "SELECT tts_asset_key,dub_skip FROM segments WHERE id=?1",
        [segment_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
}

/// Returns the asset if its row exists **and** its file is present with the
/// recorded sha256. A missing or corrupted file is treated as a cache miss.
pub fn verified_asset(
    conn: &Connection,
    store: &AssetStore,
    key: &str,
) -> Result<Option<TtsAsset>, TtsError> {
    let Some(asset) = get_asset(conn, key).map_err(TtsError::db)? else {
        return Ok(None);
    };
    let path = store.absolute(&asset.path)?;
    match std::fs::read(&path) {
        Ok(bytes) if sha256_hex(&bytes) == asset.sha256 => Ok(Some(asset)),
        _ => Ok(None),
    }
}

/// Public resolver for placement/mix: a Segment's current TTS clip as an
/// absolute path plus duration/format. `Ok(None)` when the Segment has no
/// clip yet. Fails if the recorded file is missing (no silent fallback).
pub fn resolve_segment_tts_asset(
    db: &MediaDb,
    store: &AssetStore,
    segment_id: i64,
) -> Result<Option<ResolvedTtsAsset>, TtsError> {
    let conn = db.connection();
    let Some((key, dub_skip)) = segment_dub_fields(conn, segment_id).map_err(TtsError::db)? else {
        return Err(TtsError::new("not_found", "segment not found", false));
    };
    let Some(key) = key else {
        return Ok(None);
    };
    let asset = get_asset(conn, &key)
        .map_err(TtsError::db)?
        .ok_or_else(|| TtsError::new("asset_missing", "TTS asset row is missing", true))?;
    let path = store.absolute(&asset.path)?;
    if !path.is_file() {
        return Err(TtsError::new(
            "asset_missing",
            "TTS asset file is missing; synthesize the sentence again",
            true,
        ));
    }
    Ok(Some(ResolvedTtsAsset {
        segment_id,
        key: asset.key,
        path,
        sha256: asset.sha256,
        duration_ms: asset.duration_ms,
        sample_rate: asset.sample_rate,
        channels: asset.channels,
        provider_id: asset.provider_id,
        model_id: asset.model_id,
        voice_profile_id: asset.voice_profile_id,
        dub_skip,
    }))
}

/// Toggles `dub_skip` with a revision check. Skip affects placement/mix only.
pub fn set_dub_skip(
    db: &MediaDb,
    segment_id: i64,
    expected_revision: i64,
    skip: bool,
) -> Result<Segment, TtsError> {
    let changed = db
        .connection()
        .execute(
            "UPDATE segments SET dub_skip=?1,dirty_mix=1,revision=revision+1
             WHERE id=?2 AND revision=?3",
            params![skip, segment_id, expected_revision],
        )
        .map_err(TtsError::db)?;
    if changed == 0 {
        return Err(conflict_or_missing(db, segment_id));
    }
    db.get_segment(segment_id)
        .map_err(TtsError::db)?
        .ok_or_else(|| TtsError::new("not_found", "segment not found", false))
}

fn conflict_or_missing(db: &MediaDb, segment_id: i64) -> TtsError {
    match db.get_segment(segment_id) {
        Ok(Some(_)) => TtsError::new(
            "revision_conflict",
            "the sentence changed; reload it and try again",
            false,
        ),
        _ => TtsError::new("not_found", "segment not found", false),
    }
}

// ----- orchestration -----

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SynthesisOutcome {
    pub segment: Segment,
    pub asset: TtsAsset,
    /// Absolute path of the clip (frontend: `convertFileSrc`).
    pub path: String,
    pub cache_hit: bool,
}

/// What a synthesis would use; computed under the DB lock, used without it.
#[derive(Debug, Clone, PartialEq)]
pub struct PreparedSynthesis {
    pub segment: Segment,
    pub key: String,
    pub request: TtsRequest,
    pub provenance: String,
    pub provider_id: String,
    pub model_id: String,
    pub voice_profile_id: String,
}

/// Validates the Segment and builds the request + cache key. The text is the
/// Segment's translation; there is no fallback to the source text.
pub fn prepare(
    db: &MediaDb,
    segment_id: i64,
    expected_revision: i64,
    profile: &VoiceProfile,
    adapter_version: &str,
    take: u32,
) -> Result<PreparedSynthesis, TtsError> {
    let segment = db
        .get_segment(segment_id)
        .map_err(TtsError::db)?
        .ok_or_else(|| TtsError::new("not_found", "segment not found", false))?;
    if segment.revision != expected_revision {
        return Err(TtsError::new(
            "revision_conflict",
            format!(
                "expected r{expected_revision}, sentence is at r{}",
                segment.revision
            ),
            false,
        ));
    }
    let (_, dub_skip) = segment_dub_fields(db.connection(), segment_id)
        .map_err(TtsError::db)?
        .unwrap_or((None, false));
    if dub_skip {
        return Err(TtsError::input("sentence is marked skip for dubbing"));
    }
    if segment.dirty_translation {
        return Err(TtsError::input(
            "the translation is out of date with the source; translate or edit it first",
        ));
    }
    let text = normalize_text(&segment.translated_text);
    if text.is_empty() {
        return Err(TtsError::input("sentence has no translation to synthesize"));
    }
    let params_json =
        voice::normalize_params(Some(&profile.params_json)).map_err(TtsError::input)?;
    let params: Value =
        serde_json::from_str(&params_json).map_err(|e| TtsError::input(e.to_string()))?;
    let voice_hash = voice::voice_profile_hash(profile);
    let language = profile.language.trim().to_string();
    let key = cache_key(&CacheKeyInput {
        provider_id: &profile.provider_id,
        model_id: &profile.model_id,
        adapter_version,
        normalized_text: &text,
        language: &language,
        voice_profile_hash: &voice_hash,
        params_json: &params_json,
        take,
    });
    Ok(PreparedSynthesis {
        provenance: format!("tts:{}:{}", profile.provider_id, profile.model_id),
        provider_id: profile.provider_id.clone(),
        model_id: profile.model_id.clone(),
        voice_profile_id: profile.id.clone(),
        request: TtsRequest {
            contract_version: TTS_CONTRACT_VERSION,
            model_id: profile.model_id.clone(),
            text,
            language,
            voice_kind: profile.kind.clone(),
            preset_voice: profile.preset_voice.clone(),
            design_prompt: if profile.kind == "design" {
                profile.design_prompt.clone()
            } else {
                String::new()
            },
            params,
            take,
        },
        key,
        segment,
    })
}

/// Points the Segment at the asset: `tts_asset_key`, `tts_provenance`,
/// `dirty_tts=0`, `dirty_mix=1`, revision bump. Fails on revision mismatch;
/// the asset itself stays in the cache (it is content-addressed).
pub fn commit_segment(
    db: &MediaDb,
    prepared: &PreparedSynthesis,
    expected_revision: i64,
) -> Result<Segment, TtsError> {
    let (current_key, _) = segment_dub_fields(db.connection(), prepared.segment.id)
        .map_err(TtsError::db)?
        .unwrap_or((None, false));
    let segment = &prepared.segment;
    if current_key.as_deref() == Some(prepared.key.as_str())
        && !segment.dirty_tts
        && segment.tts_provenance == prepared.provenance
        && segment.revision == expected_revision
    {
        // Already pointing at this exact clip: nothing to write.
        if let Some(fresh) = db.get_segment(segment.id).map_err(TtsError::db)? {
            if fresh.revision == expected_revision {
                return Ok(fresh);
            }
        }
    }
    let changed = db
        .connection()
        .execute(
            "UPDATE segments
             SET tts_asset_key=?1,
                 tts_provenance=?2,
                 dirty_tts=0,
                 dirty_mix=1,
                 revision=revision+1
             WHERE id=?3 AND revision=?4",
            params![
                prepared.key,
                prepared.provenance,
                segment.id,
                expected_revision
            ],
        )
        .map_err(TtsError::db)?;
    if changed == 0 {
        return Err(conflict_or_missing(db, segment.id));
    }
    db.get_segment(segment.id)
        .map_err(TtsError::db)?
        .ok_or_else(|| TtsError::new("not_found", "segment not found", false))
}

fn lock(db: &Mutex<MediaDb>) -> Result<std::sync::MutexGuard<'_, MediaDb>, TtsError> {
    db.lock()
        .map_err(|e| TtsError::new("db", e.to_string(), true))
}

/// Single-sentence synthesis. Holds the DB lock only for short synchronous
/// phases, never across the adapter call.
pub async fn run_synthesis<A: TtsAdapter>(
    db: &Mutex<MediaDb>,
    store: &AssetStore,
    adapter: &A,
    segment_id: i64,
    expected_revision: i64,
    profile: &VoiceProfile,
    take: u32,
    sink: &dyn ProgressSink,
) -> Result<SynthesisOutcome, TtsError> {
    sink.progress("prepare", 0, 3);
    let (prepared, cached) = {
        let guard = lock(db)?;
        let prepared = prepare(
            &guard,
            segment_id,
            expected_revision,
            profile,
            adapter.adapter_version(),
            take,
        )?;
        let cached = verified_asset(guard.connection(), store, &prepared.key)?;
        (prepared, cached)
    };

    let (asset, cache_hit) = match cached {
        Some(asset) => (asset, true),
        None => {
            check_cancelled(sink)?;
            sink.progress("synthesize", 1, 3);
            let raw = adapter.synthesize(&prepared.request, sink).await?;
            check_cancelled(sink)?;
            let (bytes, info) = normalize_wav(&raw)?;
            let relative = AssetStore::relative_path(&prepared.key)?;
            store.write_atomic(&relative, &bytes)?;
            let asset = TtsAsset {
                key: prepared.key.clone(),
                path: relative,
                sha256: sha256_hex(&bytes),
                duration_ms: info.duration_ms,
                sample_rate: info.sample_rate as i64,
                channels: info.channels as i64,
                provider_id: prepared.provider_id.clone(),
                model_id: prepared.model_id.clone(),
                voice_profile_id: prepared.voice_profile_id.clone(),
                created_at: String::new(),
            };
            let guard = lock(db)?;
            upsert_asset(guard.connection(), &asset).map_err(TtsError::db)?;
            let stored = get_asset(guard.connection(), &asset.key)
                .map_err(TtsError::db)?
                .unwrap_or(asset);
            (stored, false)
        }
    };

    sink.progress("commit", 2, 3);
    let segment = {
        let guard = lock(db)?;
        commit_segment(&guard, &prepared, expected_revision)?
    };
    sink.progress("done", 3, 3);
    Ok(SynthesisOutcome {
        path: store.absolute(&asset.path)?.to_string_lossy().into_owned(),
        segment,
        asset,
        cache_hit,
    })
}

// ----- Tauri commands -----

/// `<app data>/assets`.
pub fn asset_store(app: &tauri::AppHandle) -> Result<AssetStore, String> {
    use tauri::Manager;
    let dir = app.path().app_data_dir().map_err(|e| e.to_string())?;
    Ok(AssetStore::new(dir.join("assets")))
}

fn resolve_tts_target(db: &MediaDb, profile: &VoiceProfile) -> Result<ProviderConfig, TtsError> {
    let provider = db
        .get_provider(&profile.provider_id)
        .map_err(TtsError::db)?
        .ok_or_else(|| TtsError::new("provider_unavailable", "TTS Provider not found", false))?;
    if !provider.enabled {
        return Err(TtsError::new(
            "provider_unavailable",
            "TTS Provider is disabled",
            false,
        ));
    }
    let model = db
        .list_provider_models(&provider.id)
        .map_err(TtsError::db)?
        .into_iter()
        .find(|model| model.model_id == profile.model_id)
        .ok_or_else(|| {
            TtsError::new(
                "provider_unavailable",
                "model not found in Provider inventory",
                false,
            )
        })?;
    if !model.available {
        return Err(TtsError::new(
            "provider_unavailable",
            "model is currently unavailable",
            false,
        ));
    }
    if !model
        .effective_capabilities
        .iter()
        .any(|capability| capability == "speech.tts")
    {
        return Err(TtsError::new(
            "provider_unavailable",
            "selected model does not resolve speech.tts",
            false,
        ));
    }
    Ok(provider)
}

#[tauri::command]
pub async fn synthesize_segment(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
    segment_id: i64,
    expected_revision: i64,
    voice_profile_id: String,
    take: Option<u32>,
) -> Result<SynthesisOutcome, String> {
    let store = asset_store(&app)?;
    let (profile, provider) = {
        let db = state.db.lock().map_err(|e| e.to_string())?;
        let profile = voice::get(db.connection(), voice_profile_id.trim())
            .map_err(|e| e.to_string())?
            .ok_or_else(|| "voice profile not found".to_string())?;
        let provider = resolve_tts_target(&db, &profile).map_err(|e| e.to_string())?;
        (profile, provider)
    };
    let secret = crate::provider_secret(&provider)?;
    let adapter = adapter_for_provider(&provider, secret).map_err(|e| e.to_string())?;
    run_synthesis(
        &state.db,
        &store,
        &adapter,
        segment_id,
        expected_revision,
        &profile,
        take.unwrap_or(0),
        &NoopProgress,
    )
    .await
    .map_err(|e| e.to_string())
}

#[tauri::command]
pub fn get_tts_asset(
    app: tauri::AppHandle,
    state: tauri::State<'_, crate::AppState>,
    segment_id: i64,
) -> Result<Option<ResolvedTtsAsset>, String> {
    let store = asset_store(&app)?;
    let db = state.db.lock().map_err(|e| e.to_string())?;
    resolve_segment_tts_asset(&db, &store, segment_id).map_err(|e| e.to_string())
}

#[tauri::command]
pub fn set_segment_dub_skip(
    state: tauri::State<'_, crate::AppState>,
    segment_id: i64,
    expected_revision: i64,
    skip: bool,
) -> Result<Segment, String> {
    let db = state.db.lock().map_err(|e| e.to_string())?;
    set_dub_skip(&db, segment_id, expected_revision, skip).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
