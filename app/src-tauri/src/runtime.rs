//! Runtime / model packs: manifest, install plan, install transaction, doctor.
//!
//! Contract: docs/06-runtime-bootstrap.md. Everything lives under
//! <app data>/runtime/:
//!
//! ~~~text
//! runtime/
//!   active.json              pack id -> active version (+ previous known-good)
//!   staging/<uuid>.part      download in progress (Range-resumable)
//!   staging/<uuid>.json      which pack/version/sha256 that .part belongs to
//!   staging/<uuid>.dir/      unpacked, not yet probed/activated
//!   packs/<id>/<version>/    activated; never modified in place
//! ~~~
//!
//! Transaction: download -> size + sha256 -> unpack (traversal-safe) -> probe ->
//! rename into packs/ -> rewrite active.json (tmp + rename). Any failure
//! before the active.json rename leaves the previous active version intact.
//! Retryable failures keep the .part so the next attempt resumes.

use crate::jobs::{Job, JobError, JobHandle, JobRunner, JobsState};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

pub const INSTALL_JOB: &str = "runtime.install";

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
const SLASHES: &str = "//";

// ----- Manifest -----

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PackKind {
    Runtime,
    Model,
    /// Shared libraries loaded by another pack's runtime (no executable).
    Library,
}

/// zip: unpacked into the version directory. file: the download itself is
/// the artifact and is moved to entry.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PackFormat {
    Zip,
    File,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Pack {
    pub id: String,
    pub version: String,
    pub kind: PackKind,
    /// <os>-<arch> as std::env::consts spells it, or any.
    pub platform: String,
    pub format: PackFormat,
    /// Tried in order; the same bytes (same sha256) from every source, so a
    /// partial download from one resumes against the next.
    pub sources: Vec<String>,
    pub size: u64,
    pub sha256: String,
    pub unpacked_size: u64,
    /// Path inside the version directory: the executable (runtime) or the
    /// model file (model).
    pub entry: String,
    pub license: String,
    #[serde(default)]
    pub requires: Vec<String>,
    #[serde(default)]
    pub hardware: Option<String>,
}

impl Pack {
    pub fn supported_here(&self) -> bool {
        self.platform == "any" || self.platform == current_platform()
    }

    /// Peak bytes on disk while installing: the archive plus its unpacked tree.
    /// A file pack is renamed into place, so it counts once.
    pub fn required_bytes(&self) -> u64 {
        match self.format {
            PackFormat::Zip => self.size.saturating_add(self.unpacked_size),
            PackFormat::File => self.size,
        }
    }
}

pub fn current_platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

/// Release tag the faster-whisper pack archives are attached to.
/// Published 2026-10-29 as a GitHub pre-release with the BASE and CUDA
/// archives (built by scripts/runtime/lock_faster_whisper.py --build); the
/// asset digests match the pins below.
const FW_RELEASE_TAG: &str = "runtime-faster-whisper-1.2.1-1";
/// GitHub release carrying runtime.funasr (one release per pack version).
const FUNASR_RELEASE_TAG: &str = "runtime-funasr-1.4.16-1";
/// GitHub release carrying runtime.edge-tts (cloud voice; opt-in only).
const EDGE_TTS_RELEASE_TAG: &str = "runtime-edge-tts-7.3.1-1";

/// Offline mirror tried before the release: a directory (or file:/http(s): URL
/// base) holding the pack archives under their release file names, e.g. the
/// OUT directory of scripts/runtime/lock_faster_whisper.py --build. The
/// pinned sha256 still has to match.
pub const ENV_RUNTIME_MIRROR: &str = "YUZUKI_RUNTIME_MIRROR";

fn mirror_source(mirror: &str, file: &str) -> Option<String> {
    let mirror = mirror.trim().trim_end_matches(['/', '\\']);
    if mirror.is_empty() {
        return None;
    }
    if mirror.starts_with("http:") || mirror.starts_with("https:") || mirror.starts_with("file:") {
        return Some(format!("{mirror}/{file}"));
    }
    let path = mirror.replace('\\', "/");
    let path = path.trim_start_matches('/');
    Some(format!("file:{SLASHES}/{path}/{file}"))
}

fn fw_release_sources(file: &str) -> Vec<String> {
    release_sources(FW_RELEASE_TAG, file)
}

fn funasr_release_sources(file: &str) -> Vec<String> {
    release_sources(FUNASR_RELEASE_TAG, file)
}

fn edge_tts_release_sources(file: &str) -> Vec<String> {
    release_sources(EDGE_TTS_RELEASE_TAG, file)
}

/// Mirror (when set) first, then the GitHub release that publishes the pack.
fn release_sources(tag: &str, file: &str) -> Vec<String> {
    let mut sources = Vec::new();
    if let Some(source) = std::env::var(ENV_RUNTIME_MIRROR)
        .ok()
        .and_then(|mirror| mirror_source(&mirror, file))
    {
        sources.push(source);
    }
    sources.push(format!(
        "https:{SLASHES}github{}/avabbbb/Yuzuki/releases/download/{tag}/{file}",
        ".com"
    ));
    sources
}

pub fn builtin_catalog() -> Vec<Pack> {
    vec![
        // BASE: embeddable CPython 3.12 + faster-whisper 1.2.1 / CTranslate2
        // 4.8.3 and their CPU deps (av and onnxruntime included: faster_whisper
        // imports av at module load, and vad_filter uses Silero VAD).
        // Locked by workers/asr/requirements-faster-whisper-windows.lock.
        Pack {
            id: FASTER_WHISPER_RUNTIME.into(),
            version: "1.2.1-1".into(),
            kind: PackKind::Runtime,
            platform: "windows-x86_64".into(),
            format: PackFormat::Zip,
            sources: fw_release_sources("runtime.faster-whisper-1.2.1-1-windows-x86_64.zip"),
            size: 97_115_945,
            sha256: "c2af2117d19988f55c4b331037cb650ebee294e9063890625da62c5ddc0e5d7b".into(),
            unpacked_size: 271_996_257,
            entry: "python.exe".into(),
            license: "MIT (faster-whisper, CTranslate2); PSF-2.0 (CPython); deps under their own licenses".into(),
            requires: vec![],
            hardware: None,
        },
        // Optional NVIDIA GPU: cuBLAS 12.9.2.10, cuDNN 9.27.0.42, NVRTC 12.9.86
        // (cuBLASLt and cuDNN load NVRTC). Locked by
        // workers/asr/requirements-faster-whisper-cuda-windows.lock. CPU works
        // without it; device=cuda fails when it is missing.
        Pack {
            id: FASTER_WHISPER_CUDA.into(),
            version: "1.2.1-1".into(),
            kind: PackKind::Library,
            platform: "windows-x86_64".into(),
            format: PackFormat::Zip,
            sources: fw_release_sources(
                "runtime.faster-whisper.cuda-1.2.1-1-windows-x86_64.zip",
            ),
            size: 1_372_642_304,
            sha256: "2b7fd0ea278a93027b448aa976fde62c6aa92771232e8cdaaf294d4e7b4fa7a9".into(),
            unpacked_size: 2_108_417_417,
            entry: "nvidia/cublas/bin/cublas64_12.dll".into(),
            license: "NVIDIA Software License Agreement (redistributable CUDA/cuDNN components)".into(),
            requires: vec![FASTER_WHISPER_RUNTIME.into()],
            hardware: Some(HARDWARE_NVIDIA_CUDA.into()),
        },
        // SenseVoice/FunASR: embeddable CPython 3.12 + funasr 1.4.16, PyTorch
        // 2.14.1 (CPU) / torchaudio 2.11.0 and their deps; models (SenseVoice
        // Small, FSMN-VAD) download through ModelScope on first use. Locked by
        // workers/asr/requirements-funasr-windows.lock.
        Pack {
            id: FUNASR_RUNTIME.into(),
            version: "1.4.16-1".into(),
            kind: PackKind::Runtime,
            platform: "windows-x86_64".into(),
            format: PackFormat::Zip,
            sources: funasr_release_sources("runtime.funasr-1.4.16-1-windows-x86_64.zip"),
            size: 315_586_952,
            sha256: "c126079b1ee52fe822de303277674cf4d91864413fab2d3d0ec40ed6e28ccf82".into(),
            unpacked_size: 1_048_408_693,
            entry: "python.exe".into(),
            license: "MIT (FunASR); BSD-3-Clause (PyTorch); PSF-2.0 (CPython); deps under their own licenses".into(),
            requires: vec![],
            hardware: None,
        },
        // Edge TTS (CLOUD voice): embeddable CPython 3.12.0 + edge-tts 7.3.1
        // and its deps (aiohttp, certifi). The pack installs locally, but the
        // worker sends sentence text to Microsoft's online Edge speech
        // service. Opt-in only: planned for the edge-tts provider alone.
        Pack {
            id: EDGE_TTS_RUNTIME.into(),
            version: "7.3.1-1".into(),
            kind: PackKind::Runtime,
            platform: "windows-x86_64".into(),
            format: PackFormat::Zip,
            sources: edge_tts_release_sources("runtime.edge-tts-7.3.1-1-windows-x86_64.zip"),
            size: 12_476_253,
            sha256: "2aad11f88b8d7f2e25bfe640729dba0e6a9220e507c989c4b72836f57ba16349".into(),
            unpacked_size: 26_443_738,
            entry: "python.exe".into(),
            license: "LGPL-3.0 (edge-tts); Apache-2.0 (aiohttp); MPL-2.0 (certifi); PSF-2.0 (CPython); deps under their own licenses".into(),
            requires: vec![],
            hardware: None,
        },
    ]
}

/// Dependency-first, de-duplicated closure of ids over requires.
pub fn expand(catalog: &[Pack], ids: &[&str]) -> Result<Vec<Pack>, String> {
    fn visit(
        catalog: &[Pack],
        id: &str,
        visiting: &mut Vec<String>,
        out: &mut Vec<Pack>,
    ) -> Result<(), String> {
        if out.iter().any(|pack| pack.id == id) {
            return Ok(());
        }
        if visiting.iter().any(|seen| seen == id) {
            return Err(format!("pack dependency cycle at {id}"));
        }
        let pack = catalog
            .iter()
            .find(|pack| pack.id == id)
            .ok_or_else(|| format!("unknown pack {id}"))?;
        visiting.push(id.to_string());
        for dep in &pack.requires {
            visit(catalog, dep, visiting, out)?;
        }
        visiting.pop();
        out.push(pack.clone());
        Ok(())
    }
    let mut out = Vec::new();
    let mut visiting = Vec::new();
    for id in ids {
        visit(catalog, id, &mut visiting, &mut out)?;
    }
    Ok(out)
}

// ----- Provider -> packs -----

/// Packs a built-in provider needs for model_id. None: not pack-managed.
pub fn provider_pack_ids(provider_id: &str, model_id: &str) -> Option<Vec<&'static str>> {
    provider_pack_ids_for(
        provider_id,
        model_id,
        hardware_present(HARDWARE_NVIDIA_CUDA),
    )
}

/// nvidia: the host has an NVIDIA GPU; only then is the optional GPU pack
/// planned next to the faster-whisper BASE pack.
pub fn provider_pack_ids_for(
    provider_id: &str,
    _model_id: &str,
    nvidia: bool,
) -> Option<Vec<&'static str>> {
    match provider_id {
        "local.faster-whisper" if nvidia => Some(vec![FASTER_WHISPER_RUNTIME, FASTER_WHISPER_CUDA]),
        "local.faster-whisper" => Some(vec![FASTER_WHISPER_RUNTIME]),
        // CPU PyTorch; SenseVoice Small runs well below real time on CPU.
        "local.sensevoice" => Some(vec![FUNASR_RUNTIME]),
        // Cloud voice; only ever planned when the user picks Edge TTS.
        crate::providers::EDGE_TTS_PROVIDER_ID => Some(vec![EDGE_TTS_RUNTIME]),
        _ => None,
    }
}

// ----- Storage root and active index -----

static ROOT: OnceLock<PathBuf> = OnceLock::new();
static INSTALL_LOCK: Mutex<()> = Mutex::new(());

/// Called once from app setup with <app data>/runtime.
pub fn init_root(root: PathBuf) {
    let _ = ROOT.set(root);
}

pub fn root() -> Option<&'static Path> {
    ROOT.get().map(PathBuf::as_path)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ActiveIndex {
    #[serde(default)]
    pub packs: BTreeMap<String, ActiveEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ActiveEntry {
    pub version: String,
    pub entry: String,
    pub sha256: String,
    /// Previous known-good version, kept on disk.
    #[serde(default)]
    pub previous: Option<String>,
}

pub fn pack_dir(root: &Path, id: &str, version: &str) -> PathBuf {
    root.join("packs").join(id).join(version)
}

fn active_path(root: &Path) -> PathBuf {
    root.join("active.json")
}

pub fn read_active(root: &Path) -> ActiveIndex {
    fs::read(active_path(root))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default()
}

fn write_active(root: &Path, index: &ActiveIndex) -> io::Result<()> {
    fs::create_dir_all(root)?;
    let tmp = root.join(format!("active.json.{}.tmp", uuid::Uuid::new_v4()));
    let bytes = serde_json::to_vec_pretty(index).map_err(io::Error::other)?;
    {
        let mut file = File::create(&tmp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    fs::rename(&tmp, active_path(root)).inspect_err(|_| {
        let _ = fs::remove_file(&tmp);
    })
}

/// Entry file of the active version, if it is on disk.
pub fn active_entry_path(root: &Path, id: &str) -> Option<PathBuf> {
    let index = read_active(root);
    let active = index.packs.get(id)?;
    let path = pack_dir(root, id, &active.version).join(&active.entry);
    path.exists().then_some(path)
}

/// Pack ids retired by ADR-019 (whisper.cpp runtime and its model packs).
/// Startup removes their directories and active.json entries; nothing else
/// under the runtime root is touched.
pub const RETIRED_PACK_IDS: &[&str] = &[
    "whisper.cpp",
    "whisper.cpp.model.base",
    "whisper.cpp.model.small",
];

/// Deletes packs/<id> (every version) and the pack's active.json entry.
/// Returns true when anything was removed.
pub fn remove_pack(root: &Path, id: &str) -> io::Result<bool> {
    let invalid = id.is_empty() || id == "." || id == ".." || id.contains(['/', '\\']);
    if invalid {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("invalid pack id {id:?}"),
        ));
    }
    let _guard = INSTALL_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut index = read_active(root);
    let had_entry = index.packs.remove(id).is_some();
    if had_entry {
        write_active(root, &index)?;
    }
    let dir = root.join("packs").join(id);
    let had_dir = dir.exists();
    if had_dir {
        fs::remove_dir_all(&dir)?;
    }
    Ok(had_entry || had_dir)
}

/// Removes every retired pack; returns the ids that were actually removed.
pub fn remove_retired_packs(root: &Path) -> io::Result<Vec<&'static str>> {
    let mut removed = Vec::new();
    for id in RETIRED_PACK_IDS {
        if remove_pack(root, id)? {
            removed.push(*id);
        }
    }
    Ok(removed)
}

fn is_installed(root: &Path, index: &ActiveIndex, pack: &Pack) -> bool {
    index.packs.get(&pack.id).is_some_and(|active| {
        active.version == pack.version
            && pack_dir(root, &pack.id, &active.version)
                .join(&active.entry)
                .exists()
    })
}

// ----- Plan -----

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PlanItem {
    pub id: String,
    pub version: String,
    pub kind: PackKind,
    pub license: String,
    pub size: u64,
    pub unpacked_size: u64,
    pub required_bytes: u64,
    pub installed: bool,
    pub supported: bool,
    pub active_version: Option<String>,
    pub hardware: Option<String>,
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct InstallPlan {
    pub provider_id: String,
    pub model_id: String,
    pub items: Vec<PlanItem>,
    /// Ids still to install, dependency-first.
    pub missing: Vec<String>,
    pub download_bytes: u64,
    pub required_bytes: u64,
    /// None when the platform query is unavailable.
    pub free_bytes: Option<u64>,
    pub fits: Option<bool>,
    pub ready: bool,
    pub blocker: Option<String>,
}

pub fn plan_packs(
    root: Option<&Path>,
    catalog: &[Pack],
    ids: &[&str],
    provider_id: &str,
    model_id: &str,
) -> Result<InstallPlan, String> {
    let packs = expand(catalog, ids)?;
    let index = root.map(read_active).unwrap_or_default();
    let items: Vec<PlanItem> = packs
        .iter()
        .map(|pack| PlanItem {
            id: pack.id.clone(),
            version: pack.version.clone(),
            kind: pack.kind,
            license: pack.license.clone(),
            size: pack.size,
            unpacked_size: pack.unpacked_size,
            required_bytes: pack.required_bytes(),
            installed: root.is_some_and(|root| is_installed(root, &index, pack)),
            supported: pack.supported_here(),
            active_version: index.packs.get(&pack.id).map(|a| a.version.clone()),
            hardware: pack.hardware.clone(),
            sources: pack.sources.clone(),
        })
        .collect();
    let pending: Vec<&PlanItem> = items.iter().filter(|item| !item.installed).collect();
    let download_bytes = pending.iter().map(|item| item.size).sum();
    let required_bytes = pending.iter().map(|item| item.required_bytes).sum();
    let free_bytes = root.and_then(free_space);
    let fits = free_bytes.map(|free| free >= required_bytes);
    let blocker = if let Some(item) = pending.iter().find(|item| !item.supported) {
        Some(format!(
            "{} {} is not available for {}",
            item.id,
            item.version,
            current_platform()
        ))
    } else if fits == Some(false) {
        Some(format!(
            "Not enough disk space: {required_bytes} bytes needed, {} free",
            free_bytes.unwrap_or(0)
        ))
    } else if root.is_none() {
        Some("runtime storage is not initialised".into())
    } else {
        None
    };
    Ok(InstallPlan {
        provider_id: provider_id.into(),
        model_id: model_id.into(),
        missing: pending.iter().map(|item| item.id.clone()).collect(),
        ready: pending.is_empty(),
        items,
        download_bytes,
        required_bytes,
        free_bytes,
        fits,
        blocker,
    })
}

#[cfg(windows)]
fn free_space(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    #[link(name = "kernel32")]
    extern "system" {
        fn GetDiskFreeSpaceExW(
            directory: &u16,
            free_to_caller: &mut u64,
            total: Option<&mut u64>,
            total_free: Option<&mut u64>,
        ) -> i32;
    }
    let mut existing = path;
    while !existing.exists() {
        existing = existing.parent()?;
    }
    let wide: Vec<u16> = existing
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut available = 0u64;
    // SAFETY: wide is NUL-terminated and outlives the call; the out
    // reference is a valid ULARGE_INTEGER; None passes NULL for the others.
    let ok = unsafe { GetDiskFreeSpaceExW(&wide[0], &mut available, None, None) };
    (ok != 0).then_some(available)
}

#[cfg(not(windows))]
fn free_space(_path: &Path) -> Option<u64> {
    None
}

// ----- Errors -----

#[derive(Debug, Clone, PartialEq)]
pub struct InstallError {
    pub code: &'static str,
    pub retryable: bool,
    pub message: String,
}

impl InstallError {
    fn new(code: &'static str, retryable: bool, message: impl Into<String>) -> Self {
        Self {
            code,
            retryable,
            message: message.into(),
        }
    }

    fn cancelled() -> Self {
        Self::new("cancelled", true, "cancelled")
    }
}

impl From<io::Error> for InstallError {
    fn from(error: io::Error) -> Self {
        Self::new("io_error", true, error.to_string())
    }
}

impl From<InstallError> for JobError {
    fn from(error: InstallError) -> Self {
        if error.code == "cancelled" {
            JobError::cancelled()
        } else {
            JobError::new(error.code, error.retryable, error.message)
        }
    }
}

// ----- Fetch -----

pub struct Opened {
    /// Offset the body starts at: the requested offset when the source honoured
    /// the range, else 0 (the caller restarts the file).
    pub start: u64,
    pub reader: Box<dyn Read + Send>,
}

pub trait Fetcher: Send + Sync {
    fn open(&self, url: &str, offset: u64) -> Result<Opened, String>;
}

/// HTTP(S) with Range, plus file: URLs (fixtures, offline mirrors).
pub struct HttpFetcher {
    client: reqwest::Client,
}

impl HttpFetcher {
    pub fn new() -> Result<Self, String> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(20))
            // A body that stalls mid-download must fail (retryable, .part kept)
            // so cancel is observed and INSTALL_LOCK is released.
            .read_timeout(Duration::from_secs(30))
            .user_agent(concat!("YUZUKI/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(Self { client })
    }
}

fn file_url_path(url: &str) -> Option<PathBuf> {
    let rest = url.strip_prefix("file:")?.strip_prefix(SLASHES)?;
    // file:///C:/x -> C:/x ; file:///tmp/x -> /tmp/x
    let rest = rest
        .strip_prefix('/')
        .filter(|tail| tail.as_bytes().get(1) == Some(&b':'))
        .unwrap_or(rest);
    Some(PathBuf::from(rest))
}

impl Fetcher for HttpFetcher {
    fn open(&self, url: &str, offset: u64) -> Result<Opened, String> {
        if let Some(path) = file_url_path(url) {
            let mut file = File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
            let len = file.metadata().map_err(|e| e.to_string())?.len();
            let start = if offset <= len { offset } else { 0 };
            file.seek(SeekFrom::Start(start))
                .map_err(|e| e.to_string())?;
            return Ok(Opened {
                start,
                reader: Box::new(file),
            });
        }
        let mut request = self.client.get(url);
        if offset > 0 {
            request = request.header(reqwest::header::RANGE, format!("bytes={offset}-"));
        }
        // send() starts the request eagerly (it arms the timeout timer before
        // returning the future), so it must be called inside the runtime: job
        // runners are plain std threads with no Tokio context.
        let response = tauri::async_runtime::block_on(async move { request.send().await })
            .map_err(|e| e.to_string())?;
        let status = response.status();
        let start = if status == reqwest::StatusCode::PARTIAL_CONTENT {
            offset
        } else if status.is_success() {
            0
        } else {
            return Err(format!("HTTP {status}"));
        };
        Ok(Opened {
            start,
            reader: Box::new(HttpBody {
                response,
                pending: Vec::new(),
                pos: 0,
            }),
        })
    }
}

/// Streams a response body as Read, for fetches that build their own request
/// (sources::dlsite_auth::open_download).
pub fn http_body(response: reqwest::Response) -> Box<dyn Read + Send> {
    Box::new(HttpBody {
        response,
        pending: Vec::new(),
        pos: 0,
    })
}

struct HttpBody {
    response: reqwest::Response,
    pending: Vec<u8>,
    pos: usize,
}

impl Read for HttpBody {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.pending.len() {
            match tauri::async_runtime::block_on(self.response.chunk()) {
                Ok(Some(chunk)) => {
                    self.pending = chunk.to_vec();
                    self.pos = 0;
                }
                Ok(None) => return Ok(0),
                Err(error) => return Err(io::Error::other(error)),
            }
        }
        let n = buf.len().min(self.pending.len() - self.pos);
        buf[..n].copy_from_slice(&self.pending[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// Fills part up to pack.size, resuming from its current length and
/// falling through pack.sources in order. Leaves part in place on failure.
fn download(
    pack: &Pack,
    part: &Path,
    fetcher: &dyn Fetcher,
    cancel: &dyn Fn() -> bool,
    progress: &mut dyn FnMut(u64),
) -> Result<(), InstallError> {
    let mut errors = Vec::new();
    for url in &pack.sources {
        let mut have = fs::metadata(part).map(|meta| meta.len()).unwrap_or(0);
        if have > pack.size {
            OpenOptions::new().write(true).open(part)?.set_len(0)?;
            have = 0;
        }
        progress(have);
        if have == pack.size {
            return Ok(());
        }
        if cancel() {
            return Err(InstallError::cancelled());
        }
        let mut opened = match fetcher.open(url, have) {
            Ok(opened) => opened,
            Err(error) => {
                errors.push(format!("{url}: {error}"));
                continue;
            }
        };
        let mut file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(part)?;
        if opened.start == 0 {
            file.set_len(0)?;
        }
        file.seek(SeekFrom::Start(opened.start))?;
        let mut written = opened.start;
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            if cancel() {
                file.flush()?;
                return Err(InstallError::cancelled());
            }
            let n = match opened.reader.read(&mut buf) {
                Ok(n) => n,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => {
                    errors.push(format!("{url}: {error}"));
                    break;
                }
            };
            if n == 0 {
                break;
            }
            if written + n as u64 > pack.size {
                errors.push(format!("{url}: more bytes than the manifest size"));
                file.set_len(0)?;
                written = 0;
                break;
            }
            file.write_all(&buf[..n])?;
            written += n as u64;
            progress(written);
        }
        file.flush()?;
        if written == pack.size {
            return Ok(());
        }
        if !errors.last().is_some_and(|e| e.starts_with(url.as_str())) {
            errors.push(format!(
                "{url}: connection closed at {written}/{}",
                pack.size
            ));
        }
    }
    Err(InstallError::new(
        "download_failed",
        true,
        format!("{}: {}", pack.id, errors.join("; ")),
    ))
}

// ----- Verify / unpack / probe -----

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sha256_file(path: &Path, cancel: &dyn Fn() -> bool) -> Result<String, InstallError> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 256 * 1024];
    loop {
        if cancel() {
            return Err(InstallError::cancelled());
        }
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

fn verify(pack: &Pack, part: &Path, cancel: &dyn Fn() -> bool) -> Result<(), InstallError> {
    let len = fs::metadata(part)?.len();
    if len != pack.size {
        return Err(InstallError::new(
            "size_mismatch",
            true,
            format!("{}: expected {} bytes, got {len}", pack.id, pack.size),
        ));
    }
    let digest = sha256_file(part, cancel)?;
    if !digest.eq_ignore_ascii_case(&pack.sha256) {
        return Err(InstallError::new(
            "hash_mismatch",
            true,
            format!("{}: expected sha256 {}, got {digest}", pack.id, pack.sha256),
        ));
    }
    Ok(())
}

/// Relative path for an archive entry or manifest entry. Rejects ..,
/// absolute paths, drive letters / alternate streams (:) and empty names
/// instead of silently skipping them: a pack that tries is a bad pack.
pub fn safe_relative(name: &str) -> Result<PathBuf, InstallError> {
    let unsafe_path =
        || InstallError::new("unsafe_archive", false, format!("unsafe path {name:?}"));
    let normalized = name.replace('\\', "/");
    if normalized.starts_with('/') {
        return Err(unsafe_path());
    }
    let mut out = PathBuf::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => continue,
            ".." => return Err(unsafe_path()),
            part if part.contains(':') => return Err(unsafe_path()),
            part => out.push(part),
        }
    }
    if out.as_os_str().is_empty() {
        return Err(unsafe_path());
    }
    Ok(out)
}

fn extract_zip(archive: &Path, dest: &Path, limit: u64) -> Result<u64, InstallError> {
    let bad = |e: zip::result::ZipError| InstallError::new("bad_archive", false, e.to_string());
    let mut zip = zip::ZipArchive::new(File::open(archive)?).map_err(bad)?;
    // Validate every name before writing anything.
    for i in 0..zip.len() {
        let entry = zip.by_index_raw(i).map_err(bad)?;
        safe_relative(&entry.name().map_err(bad)?)?;
    }
    let mut total = 0u64;
    for i in 0..zip.len() {
        let mut entry = zip.by_index(i).map_err(bad)?;
        let rel = safe_relative(&entry.name().map_err(bad)?)?;
        let out = dest.join(rel);
        if entry.is_dir() {
            fs::create_dir_all(&out)?;
            continue;
        }
        if let Some(parent) = out.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut writer = File::create(&out)?;
        let budget = limit.saturating_sub(total).saturating_add(1);
        total += io::copy(&mut (&mut entry).take(budget), &mut writer)?;
        if total > limit {
            return Err(InstallError::new(
                "unsafe_archive",
                false,
                format!("unpacked size exceeds the manifest's {limit} bytes"),
            ));
        }
    }
    Ok(total)
}

/// (pack, unpacked version dir) -> Ok(detail) | Err(reason).
pub type Prober = dyn Fn(&Pack, &Path) -> Result<String, String> + Send + Sync;

/// Runtime: <entry> --help exits 0; the faster-whisper BASE pack instead
/// imports ctranslate2 + faster_whisper with its own interpreter. Model: the
/// sha256 was verified on download; here only the file's presence and size.
/// Library: the entry file is present (another pack's runtime loads it).
pub fn default_probe(pack: &Pack, dir: &Path) -> Result<String, String> {
    let entry = dir.join(&pack.entry);
    let meta = fs::metadata(&entry).map_err(|e| format!("{} missing: {e}", pack.entry))?;
    match pack.kind {
        PackKind::Model => {
            if pack.format == PackFormat::File && meta.len() != pack.size {
                Err(format!(
                    "{} is {} bytes, expected {}",
                    pack.entry,
                    meta.len(),
                    pack.size
                ))
            } else {
                Ok("present; sha256 verified at install".into())
            }
        }
        PackKind::Library => Ok("present; sha256 verified at install".into()),
        PackKind::Runtime if pack.id == FASTER_WHISPER_RUNTIME => run_probe(
            &entry,
            &["-c", FASTER_WHISPER_IMPORT_PROBE],
            PYTHON_PROBE_TIMEOUT,
        ),
        PackKind::Runtime if pack.id == FUNASR_RUNTIME => {
            run_probe(&entry, &["-c", FUNASR_IMPORT_PROBE], FUNASR_PROBE_TIMEOUT)
        }
        PackKind::Runtime if pack.id == EDGE_TTS_RUNTIME => {
            run_probe(&entry, &["-c", EDGE_TTS_IMPORT_PROBE], PYTHON_PROBE_TIMEOUT)
        }
        PackKind::Runtime => run_probe(&entry, &["--help"], PROBE_TIMEOUT),
    }
}

/// Imported by the BASE pack probe; cold imports of onnxruntime/av/numpy can
/// take a while on a slow disk, hence the longer timeout.
const FASTER_WHISPER_IMPORT_PROBE: &str = "import ctranslate2, faster_whisper";
const PYTHON_PROBE_TIMEOUT: Duration = Duration::from_secs(120);
/// Imported by the runtime.funasr probe: the worker's own imports. A cold
/// torch + funasr import is several times slower than faster-whisper's.
const FUNASR_IMPORT_PROBE: &str =
    "import torch, soundfile, funasr; from funasr.utils.postprocess_utils import rich_transcription_postprocess";
const FUNASR_PROBE_TIMEOUT: Duration = Duration::from_secs(240);
/// Imported by the runtime.edge-tts probe: the worker's own imports. The
/// \x5f escapes spell the dunder attribute edge_tts.<dunder>version<dunder>.
const EDGE_TTS_IMPORT_PROBE: &str =
    "import edge_tts, aiohttp, certifi; print(edge_tts.\x5f\x5fversion\x5f\x5f)";

fn run_probe(entry: &Path, args: &[&str], timeout: Duration) -> Result<String, String> {
    let shown = args.join(" ");
    let mut command = Command::new(entry);
    command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // A pack interpreter must not pick up a developer's Python environment.
    command.env_remove("PYTHONPATH").env_remove("PYTHONHOME");
    if let Some(dir) = entry.parent() {
        command.current_dir(dir);
    }
    hide_window(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", entry.display()))?;
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) if status.success() => return Ok(format!("{shown} exited 0")),
            Some(status) => return Err(format!("{shown} exited with {status}")),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{shown} timed out after {timeout:?}"));
            }
            None => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

#[cfg(windows)]
fn hide_window(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(CREATE_NO_WINDOW);
}

#[cfg(not(windows))]
fn hide_window(_command: &mut Command) {}

// ----- Staging -----

#[derive(Debug, Serialize, Deserialize)]
struct StagedMeta {
    id: String,
    version: String,
    sha256: String,
}

fn staged_matches(staging: &Path, pack: &Pack) -> Vec<String> {
    let Ok(entries) = fs::read_dir(staging) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("json"))
        .filter(|path| {
            fs::read(path)
                .ok()
                .and_then(|bytes| serde_json::from_slice::<StagedMeta>(&bytes).ok())
                .is_some_and(|meta| {
                    meta.id == pack.id
                        && meta.version == pack.version
                        && meta.sha256.eq_ignore_ascii_case(&pack.sha256)
                })
        })
        .filter_map(|path| path.file_stem().and_then(|s| s.to_str()).map(String::from))
        .collect()
}

/// Token of the leftover staging slot for this exact pack, or a fresh one.
fn staged_token(staging: &Path, pack: &Pack) -> Result<String, InstallError> {
    if let Some(token) = staged_matches(staging, pack).into_iter().next() {
        return Ok(token);
    }
    let token = uuid::Uuid::new_v4().to_string();
    let meta = StagedMeta {
        id: pack.id.clone(),
        version: pack.version.clone(),
        sha256: pack.sha256.clone(),
    };
    let bytes = serde_json::to_vec(&meta).map_err(io::Error::other)?;
    fs::write(staging.join(format!("{token}.json")), bytes)?;
    Ok(token)
}

fn discard_staged(staging: &Path, token: &str) {
    let _ = fs::remove_file(staging.join(format!("{token}.part")));
    let _ = fs::remove_dir_all(staging.join(format!("{token}.dir")));
    let _ = fs::remove_file(staging.join(format!("{token}.json")));
}

fn staged_bytes(root: &Path, pack: &Pack) -> u64 {
    let staging = root.join("staging");
    staged_matches(&staging, pack)
        .iter()
        .filter_map(|token| fs::metadata(staging.join(format!("{token}.part"))).ok())
        .map(|meta| meta.len())
        .sum()
}

fn prune(root: &Path, id: &str, keep: &[&str]) {
    let Ok(entries) = fs::read_dir(root.join("packs").join(id)) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') || keep.contains(&name.as_str()) {
            continue;
        }
        let _ = fs::remove_dir_all(entry.path());
    }
}

// ----- Install transaction -----

pub struct Installer<'a> {
    pub root: &'a Path,
    pub fetcher: &'a dyn Fetcher,
    pub prober: &'a Prober,
}

impl Installer<'_> {
    /// Installs and activates one pack. progress(stage, done, total) with
    /// stage in download | verify | unpack | probe | activate.
    pub fn install(
        &self,
        pack: &Pack,
        cancel: &dyn Fn() -> bool,
        progress: &mut dyn FnMut(&str, u64, u64),
    ) -> Result<ActiveEntry, InstallError> {
        if !pack.supported_here() {
            return Err(InstallError::new(
                "unsupported_platform",
                false,
                format!(
                    "{} is built for {}, this machine is {}",
                    pack.id,
                    pack.platform,
                    current_platform()
                ),
            ));
        }
        let staging = self.root.join("staging");
        fs::create_dir_all(&staging)?;
        let token = staged_token(&staging, pack)?;
        let part = staging.join(format!("{token}.part"));

        download(pack, &part, self.fetcher, cancel, &mut |done| {
            progress("download", done, pack.size)
        })?;
        progress("verify", 0, pack.size);
        if let Err(error) = verify(pack, &part, cancel) {
            if error.code != "cancelled" {
                // Corrupt bytes: never resume from them.
                discard_staged(&staging, &token);
            }
            return Err(error);
        }

        let dir = staging.join(format!("{token}.dir"));
        if dir.exists() {
            fs::remove_dir_all(&dir)?;
        }
        fs::create_dir_all(&dir)?;
        let outcome = self.finish(pack, &part, &dir, progress);
        match &outcome {
            Ok(_) => discard_staged(&staging, &token),
            Err(error) if !error.retryable => discard_staged(&staging, &token),
            Err(_) => {
                let _ = fs::remove_dir_all(&dir);
            }
        }
        outcome
    }

    fn finish(
        &self,
        pack: &Pack,
        part: &Path,
        dir: &Path,
        progress: &mut dyn FnMut(&str, u64, u64),
    ) -> Result<ActiveEntry, InstallError> {
        progress("unpack", 0, pack.unpacked_size);
        match pack.format {
            PackFormat::Zip => {
                extract_zip(part, dir, pack.unpacked_size.max(1))?;
            }
            PackFormat::File => {
                let target = dir.join(safe_relative(&pack.entry)?);
                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::rename(part, &target)?;
            }
        }
        progress("probe", 0, 1);
        (self.prober)(pack, dir).map_err(|reason| {
            InstallError::new("probe_failed", false, format!("{}: {reason}", pack.id))
        })?;
        progress("activate", 0, 1);
        self.activate(pack, dir)
    }

    fn activate(&self, pack: &Pack, unpacked: &Path) -> Result<ActiveEntry, InstallError> {
        let target = pack_dir(self.root, &pack.id, &pack.version);
        let parent = self.root.join("packs").join(&pack.id);
        fs::create_dir_all(&parent)?;
        let mut index = read_active(self.root);

        // Same version already on disk (repair/reinstall): move it aside and
        // put it back if anything below fails.
        let aside = if target.exists() {
            let aside = parent.join(format!(
                ".{}.replaced-{}",
                pack.version,
                uuid::Uuid::new_v4()
            ));
            fs::rename(&target, &aside)?;
            Some(aside)
        } else {
            None
        };
        let restore = |aside: &Option<PathBuf>| {
            if let Some(aside) = aside {
                let _ = fs::rename(aside, &target);
            }
        };
        if let Err(error) = fs::rename(unpacked, &target) {
            restore(&aside);
            return Err(error.into());
        }

        let previous = match index.packs.get(&pack.id) {
            Some(old) if old.version != pack.version => Some(old.version.clone()),
            Some(old) => old.previous.clone(),
            None => None,
        };
        let entry = ActiveEntry {
            version: pack.version.clone(),
            entry: pack.entry.clone(),
            sha256: pack.sha256.clone(),
            previous,
        };
        index.packs.insert(pack.id.clone(), entry.clone());
        if let Err(error) = write_active(self.root, &index) {
            let _ = fs::remove_dir_all(&target);
            restore(&aside);
            return Err(error.into());
        }
        if let Some(aside) = aside {
            let _ = fs::remove_dir_all(aside);
        }
        let mut keep = vec![entry.version.as_str()];
        if let Some(previous) = entry.previous.as_deref() {
            keep.push(previous);
        }
        prune(self.root, &pack.id, &keep);
        Ok(entry)
    }
}

// ----- Jobs -----

fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// runtime.install runner. Input: {"packs": [ids]}; it recomputes what is
/// missing, so a retry or a duplicate job resumes/skips instead of redoing.
pub fn install_runner() -> JobRunner {
    Arc::new(|job: &JobHandle| {
        let root = root().ok_or_else(|| {
            JobError::new(
                "runtime_root_missing",
                false,
                "runtime storage is not initialised",
            )
        })?;
        let ids: Vec<String> = job
            .input()
            .get("packs")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or_default();
        if ids.is_empty() {
            return Err(JobError::new("invalid_input", false, "no packs requested"));
        }
        // One install at a time: two jobs must not share a staging slot.
        let _guard = INSTALL_LOCK
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let catalog = builtin_catalog();
        let refs: Vec<&str> = ids.iter().map(String::as_str).collect();
        let packs =
            expand(&catalog, &refs).map_err(|e| JobError::new("invalid_input", false, e))?;
        let index = read_active(root);
        let todo: Vec<Pack> = packs
            .into_iter()
            .filter(|pack| !is_installed(root, &index, pack))
            .collect();
        let required: u64 = todo.iter().map(Pack::required_bytes).sum();
        if let Some(free) = free_space(root) {
            if free < required {
                return Err(JobError::new(
                    "insufficient_disk",
                    false,
                    format!("{required} bytes needed, {free} free"),
                ));
            }
        }
        let fetcher = HttpFetcher::new().map_err(|e| JobError::new("http_client", false, e))?;
        let installer = Installer {
            root,
            fetcher: &fetcher,
            prober: &default_probe,
        };
        let total: u64 = todo.iter().map(|pack| pack.size).sum();
        let mut base = 0u64;
        let mut installed = Vec::new();
        for pack in &todo {
            let cancel = || job.is_cancelled();
            let mut last: Option<(String, Instant)> = None;
            let mut report = |stage: &str, done: u64, _total: u64| {
                let overall = if stage == "download" {
                    base + done
                } else {
                    base + pack.size
                };
                let label = format!("{stage}:{}", pack.id);
                let due = match &last {
                    Some((prev, at)) => prev != &label || at.elapsed() >= PROGRESS_INTERVAL,
                    None => true,
                };
                if due || overall == total {
                    job.progress(to_i64(overall), to_i64(total), &label);
                    last = Some((label, Instant::now()));
                }
            };
            let entry = installer.install(pack, &cancel, &mut report)?;
            base += pack.size;
            installed.push(serde_json::json!({ "id": pack.id, "version": entry.version }));
        }
        Ok(serde_json::json!({ "installed": installed }))
    })
}

// ----- Readiness / resolution -----

pub fn env_lookup(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
}

/// Runtime pack holding embeddable CPython + hash-locked faster-whisper wheels.
pub const FASTER_WHISPER_RUNTIME: &str = "runtime.faster-whisper";
pub const ENV_FASTER_WHISPER_PYTHON: &str = "TSUBAME_FASTER_WHISPER_PYTHON";

/// Python interpreter for faster_whisper_worker.py: developer override first,
/// then the active runtime pack entry. Never the system Python.
pub fn resolve_faster_whisper_python_in(
    root: Option<&Path>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<PathBuf, String> {
    if let Some(value) = env(ENV_FASTER_WHISPER_PYTHON) {
        return Ok(PathBuf::from(value));
    }
    root.and_then(|root| active_entry_path(root, FASTER_WHISPER_RUNTIME))
        .ok_or_else(|| "faster-whisper runtime pack is not installed".to_string())
}

pub fn resolve_faster_whisper_python() -> Result<PathBuf, String> {
    resolve_faster_whisper_python_in(root(), &env_lookup)
}

/// Runtime pack holding embeddable CPython + hash-locked FunASR / PyTorch (CPU)
/// wheels for funasr_worker.py (SenseVoice + FSMN-VAD).
pub const FUNASR_RUNTIME: &str = "runtime.funasr";
pub const ENV_FUNASR_PYTHON: &str = "TSUBAME_FUNASR_PYTHON";

/// Python interpreter for funasr_worker.py: developer override first, then the
/// active runtime pack entry. Never the system Python.
pub fn resolve_funasr_python_in(
    root: Option<&Path>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<PathBuf, String> {
    if let Some(value) = env(ENV_FUNASR_PYTHON) {
        return Ok(PathBuf::from(value));
    }
    root.and_then(|root| active_entry_path(root, FUNASR_RUNTIME))
        .ok_or_else(|| "SenseVoice/FunASR runtime pack is not installed".to_string())
}

pub fn resolve_funasr_python() -> Result<PathBuf, String> {
    resolve_funasr_python_in(root(), &env_lookup)
}

/// Runtime pack holding embeddable CPython + hash-locked edge-tts wheels for
/// workers/tts/edge_tts_worker.py (a cloud voice: text goes to Microsoft).
pub const EDGE_TTS_RUNTIME: &str = "runtime.edge-tts";
pub const ENV_EDGE_TTS_PYTHON: &str = "TSUBAME_EDGE_TTS_PYTHON";

/// Python interpreter for edge_tts_worker.py: developer override first, then
/// the active runtime pack entry. Never the system Python.
pub fn resolve_edge_tts_python_in(
    root: Option<&Path>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<PathBuf, String> {
    if let Some(value) = env(ENV_EDGE_TTS_PYTHON) {
        return Ok(PathBuf::from(value));
    }
    root.and_then(|root| active_entry_path(root, EDGE_TTS_RUNTIME))
        .ok_or_else(|| "Edge TTS runtime pack is not installed".to_string())
}

pub fn resolve_edge_tts_python() -> Result<PathBuf, String> {
    resolve_edge_tts_python_in(root(), &env_lookup)
}

/// GPU pack: the NVIDIA cuBLAS/cuDNN/NVRTC DLLs that the BASE pack's
/// ctranslate2 loads on CUDA. Requires runtime.faster-whisper.
pub const FASTER_WHISPER_CUDA: &str = "runtime.faster-whisper.cuda";
/// Worker variable carrying the GPU pack's version directory
/// (workers/asr/faster_whisper_worker.py adds its nvidia/<lib>/bin dirs).
pub const ENV_FW_CUDA_DIR: &str = "YUZUKI_FW_CUDA_DIR";
/// Catalog hardware tag for packs that only help on an NVIDIA GPU.
pub const HARDWARE_NVIDIA_CUDA: &str = "nvidia-cuda";

/// Version directory of the active GPU pack, when it is installed.
pub fn active_faster_whisper_cuda_dir_in(root: Option<&Path>) -> Option<PathBuf> {
    let root = root?;
    active_entry_path(root, FASTER_WHISPER_CUDA)?;
    let version = read_active(root)
        .packs
        .get(FASTER_WHISPER_CUDA)?
        .version
        .clone();
    Some(pack_dir(root, FASTER_WHISPER_CUDA, &version))
}

/// GPU DLL directory for the worker: developer override (YUZUKI_FW_CUDA_DIR,
/// paired with TSUBAME_FASTER_WHISPER_PYTHON) first, then the active GPU pack.
pub fn resolve_faster_whisper_cuda_dir_in(
    root: Option<&Path>,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<PathBuf> {
    if let Some(value) = env(ENV_FW_CUDA_DIR).filter(|v| !v.trim().is_empty()) {
        return Some(PathBuf::from(value));
    }
    active_faster_whisper_cuda_dir_in(root)
}

pub fn resolve_faster_whisper_cuda_dir() -> Option<PathBuf> {
    resolve_faster_whisper_cuda_dir_in(root(), &env_lookup)
}

/// Whether this host can use packs tagged with tag. nvidia-cuda: the
/// NVIDIA driver's nvcuda.dll is installed (Windows); elsewhere false.
pub fn hardware_present(tag: &str) -> bool {
    match tag {
        HARDWARE_NVIDIA_CUDA => nvidia_driver_present(),
        _ => false,
    }
}

#[cfg(windows)]
fn nvidia_driver_present() -> bool {
    std::env::var_os("SystemRoot").is_some_and(|root| {
        Path::new(&root)
            .join("System32")
            .join("nvcuda.dll")
            .exists()
    })
}

#[cfg(not(windows))]
fn nvidia_driver_present() -> bool {
    false
}

// ----- Doctor -----

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct EnvOverride {
    pub name: String,
    pub value: String,
    pub exists: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ProbeReport {
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct PackStatus {
    pub id: String,
    pub kind: PackKind,
    pub license: String,
    pub manifest_version: String,
    /// active | broken | partial | missing | unsupported
    pub state: String,
    pub active_version: Option<String>,
    pub previous_version: Option<String>,
    pub update_available: bool,
    pub path: Option<String>,
    pub staged_bytes: u64,
    pub env_override: Option<EnvOverride>,
    pub probe: Option<ProbeReport>,
}

pub fn override_env_for(pack_id: &str) -> Option<&'static str> {
    match pack_id {
        FASTER_WHISPER_RUNTIME => Some(ENV_FASTER_WHISPER_PYTHON),
        FUNASR_RUNTIME => Some(ENV_FUNASR_PYTHON),
        EDGE_TTS_RUNTIME => Some(ENV_EDGE_TTS_PYTHON),
        _ => None,
    }
}

pub fn doctor(
    root: Option<&Path>,
    catalog: &[Pack],
    env: &dyn Fn(&str) -> Option<String>,
    prober: &Prober,
) -> Vec<PackStatus> {
    let index = root.map(read_active).unwrap_or_default();
    catalog
        .iter()
        .map(|pack| {
            let active = index.packs.get(&pack.id);
            let staged = root.map(|root| staged_bytes(root, pack)).unwrap_or(0);
            let (state, path, probe) = match (root, active) {
                (Some(root), Some(active)) => {
                    let dir = pack_dir(root, &pack.id, &active.version);
                    let mut as_active = pack.clone();
                    as_active.version = active.version.clone();
                    as_active.entry = active.entry.clone();
                    let report = match prober(&as_active, &dir) {
                        Ok(detail) => ProbeReport { ok: true, detail },
                        Err(detail) => ProbeReport { ok: false, detail },
                    };
                    let state = if report.ok { "active" } else { "broken" };
                    (
                        state,
                        Some(dir.join(&active.entry).display().to_string()),
                        Some(report),
                    )
                }
                _ => {
                    let state = if !pack.supported_here() {
                        "unsupported"
                    } else if staged > 0 {
                        "partial"
                    } else {
                        "missing"
                    };
                    (state, None, None)
                }
            };
            PackStatus {
                id: pack.id.clone(),
                kind: pack.kind,
                license: pack.license.clone(),
                manifest_version: pack.version.clone(),
                state: state.into(),
                active_version: active.map(|a| a.version.clone()),
                previous_version: active.and_then(|a| a.previous.clone()),
                update_available: active.is_some_and(|a| a.version != pack.version),
                path,
                staged_bytes: staged,
                env_override: override_env_for(&pack.id).and_then(|name| {
                    env(name).map(|value| EnvOverride {
                        name: name.into(),
                        exists: Path::new(&value).exists(),
                        value,
                    })
                }),
                probe,
            }
        })
        .collect()
}

// ----- Tauri surface -----

#[tauri::command]
pub fn runtime_install_plan(
    provider_id: String,
    model_id: Option<String>,
) -> Result<InstallPlan, String> {
    crate::operations::plan_install(&provider_id, model_id.as_deref()).map_err(|e| e.message)
}

#[tauri::command]
pub fn install_runtime_packs(
    provider_id: String,
    model_id: Option<String>,
    state: tauri::State<'_, crate::AppState>,
    jobs: tauri::State<'_, JobsState>,
) -> Result<Job, String> {
    crate::operations::invoke::<crate::operations::RuntimeInstall>(
        &crate::op_ctx(&state, &jobs),
        crate::operations::RuntimePlanInput {
            provider_id,
            model_id,
        },
    )
    .map_err(|e| e.message)
}

#[tauri::command]
pub async fn runtime_doctor(app: tauri::AppHandle) -> Result<Vec<PackStatus>, String> {
    tauri::async_runtime::spawn_blocking(move || {
        use tauri::Manager;
        let state = app.state::<crate::AppState>();
        let jobs = app.state::<crate::jobs::JobsState>();
        crate::operations::invoke::<crate::operations::RuntimeDoctor>(
            &crate::op_ctx(&state, &jobs),
            crate::operations::NoInput {},
        )
        .map_err(|e| e.message)
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests;
