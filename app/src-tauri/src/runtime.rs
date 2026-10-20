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

use crate::jobs::{Job, JobError, JobHandle, JobRunner, JobsState, NewJob};
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
pub const WHISPER_RUNTIME: &str = "whisper.cpp";
pub const WHISPER_MODEL_BASE: &str = "whisper.cpp.model.base";
pub const WHISPER_MODEL_SMALL: &str = "whisper.cpp.model.small";
/// Developer overrides. They win over active packs and are always reported
/// (doctor, ASR notes); they are never a silent fallback.
pub const ENV_WHISPER_BIN: &str = "TSUBAME_WHISPER_CPP_BIN";
pub const ENV_WHISPER_MODEL: &str = "TSUBAME_WHISPER_CPP_MODEL";

const PROBE_TIMEOUT: Duration = Duration::from_secs(15);
const PROGRESS_INTERVAL: Duration = Duration::from_millis(200);
const SLASHES: &str = "//";

// ----- Manifest -----

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PackKind {
    Runtime,
    Model,
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

fn hf_sources(file: &str) -> Vec<String> {
    // The canonical Hugging Face host is unreachable from some networks;
    // hf-mirror serves the same LFS objects (same sha256).
    [("huggingface", ".co"), ("hf-mirror", ".com")]
        .iter()
        .map(|(host, tld)| {
            format!("https:{SLASHES}{host}{tld}/ggerganov/whisper.cpp/resolve/main/{file}")
        })
        .collect()
}

pub fn builtin_catalog() -> Vec<Pack> {
    vec![
        Pack {
            id: WHISPER_RUNTIME.into(),
            version: "1.9.2".into(),
            kind: PackKind::Runtime,
            platform: "windows-x86_64".into(),
            format: PackFormat::Zip,
            sources: vec![format!(
                "https:{SLASHES}github{}/ggml-org/whisper.cpp/releases/download/v1.9.2/whisper-bin-x64{}",
                ".com", ".zip"
            )],
            size: 8_194_445,
            sha256: "49dcc16de826f20bd53d44f947a1ae49dfa81f86cad67a64d80820cb192d674a".into(),
            unpacked_size: 20_848_640,
            entry: "Release/whisper-cli.exe".into(),
            license: "MIT".into(),
            requires: vec![],
            hardware: None,
        },
        Pack {
            id: WHISPER_MODEL_BASE.into(),
            version: "1".into(),
            kind: PackKind::Model,
            platform: "any".into(),
            format: PackFormat::File,
            sources: hf_sources("ggml-base.bin"),
            size: 147_951_465,
            sha256: "60ed5bc3dd14eea856493d334349b405782ddcaf0028d4b5df4088345fba2efe".into(),
            unpacked_size: 147_951_465,
            entry: "ggml-base.bin".into(),
            license: "MIT".into(),
            requires: vec![],
            hardware: None,
        },
        Pack {
            id: WHISPER_MODEL_SMALL.into(),
            version: "1".into(),
            kind: PackKind::Model,
            platform: "any".into(),
            format: PackFormat::File,
            sources: hf_sources("ggml-small.bin"),
            size: 487_601_967,
            sha256: "1be3a9b2063867b937e64e2ec7483364a79917e157fa98c5d94b5c1fffea987b".into(),
            unpacked_size: 487_601_967,
            entry: "ggml-small.bin".into(),
            license: "MIT".into(),
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
    match provider_id {
        "local.whisper-cpp" => Some(vec![WHISPER_RUNTIME, whisper_model_pack(model_id)]),
        _ => None,
    }
}

/// small -> small; base, runtime-model and empty -> base.
pub fn whisper_model_pack(model_id: &str) -> &'static str {
    if model_id == "small" {
        WHISPER_MODEL_SMALL
    } else {
        WHISPER_MODEL_BASE
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
        let response = tauri::async_runtime::block_on(request.send()).map_err(|e| e.to_string())?;
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

/// Runtime: <entry> --help exits 0. Model: the sha256 was verified on
/// download; here only the file's presence and size.
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
        PackKind::Runtime => run_help(&entry),
    }
}

fn run_help(entry: &Path) -> Result<String, String> {
    let mut command = Command::new(entry);
    command
        .arg("--help")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = entry.parent() {
        command.current_dir(dir);
    }
    hide_window(&mut command);
    let mut child = command
        .spawn()
        .map_err(|e| format!("cannot start {}: {e}", entry.display()))?;
    let deadline = Instant::now() + PROBE_TIMEOUT;
    loop {
        match child.try_wait().map_err(|e| e.to_string())? {
            Some(status) if status.success() => return Ok("--help exited 0".into()),
            Some(status) => return Err(format!("--help exited with {status}")),
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("--help timed out after {PROBE_TIMEOUT:?}"));
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

#[derive(Debug, Clone, PartialEq)]
pub struct WhisperPaths {
    pub bin: PathBuf,
    pub model: PathBuf,
    /// Human-readable notes, e.g. that a developer override is in effect.
    pub notes: Vec<String>,
}

fn active_whisper_model(root: &Path, model_id: &str) -> Option<PathBuf> {
    match model_id {
        "base" => active_entry_path(root, WHISPER_MODEL_BASE),
        "small" => active_entry_path(root, WHISPER_MODEL_SMALL),
        // No explicit choice: whichever model pack is installed, reported in notes.
        "" | "runtime-model" => active_entry_path(root, WHISPER_MODEL_BASE)
            .or_else(|| active_entry_path(root, WHISPER_MODEL_SMALL)),
        _ => None,
    }
}

pub fn resolve_whisper_in(
    root: Option<&Path>,
    model_id: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> Result<WhisperPaths, String> {
    let mut notes = Vec::new();
    let bin = match env(ENV_WHISPER_BIN) {
        Some(value) => {
            notes.push(format!("developer override {ENV_WHISPER_BIN}={value}"));
            PathBuf::from(value)
        }
        None => root
            .and_then(|root| active_entry_path(root, WHISPER_RUNTIME))
            .ok_or_else(|| "whisper.cpp runtime pack is not installed".to_string())?,
    };
    let model = match env(ENV_WHISPER_MODEL) {
        Some(value) => {
            notes.push(format!("developer override {ENV_WHISPER_MODEL}={value}"));
            PathBuf::from(value)
        }
        None => {
            if !matches!(model_id, "" | "runtime-model" | "base" | "small") {
                return Err(format!("unknown model for local.whisper-cpp: {model_id}"));
            }
            // Empty and runtime-model are documented aliases for base, not a
            // fallback; only developer overrides are reported in notes.
            root.and_then(|root| active_whisper_model(root, model_id))
                .ok_or_else(|| {
                    format!(
                        "whisper.cpp model pack {} is not installed",
                        whisper_model_pack(model_id)
                    )
                })?
        }
    };
    Ok(WhisperPaths { bin, model, notes })
}

pub fn resolve_whisper(model_id: &str) -> Result<WhisperPaths, String> {
    resolve_whisper_in(root(), model_id, &env_lookup)
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
        WHISPER_RUNTIME => Some(ENV_WHISPER_BIN),
        WHISPER_MODEL_BASE | WHISPER_MODEL_SMALL => Some(ENV_WHISPER_MODEL),
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
    let model_id = model_id.unwrap_or_default();
    let ids = provider_pack_ids(&provider_id, &model_id)
        .ok_or_else(|| format!("{provider_id} has no installable packs"))?;
    plan_packs(root(), &builtin_catalog(), &ids, &provider_id, &model_id)
}

#[tauri::command]
pub fn install_runtime_packs(
    provider_id: String,
    model_id: Option<String>,
    state: tauri::State<'_, JobsState>,
) -> Result<Job, String> {
    let plan = runtime_install_plan(provider_id.clone(), model_id.clone())?;
    if let Some(blocker) = plan.blocker {
        return Err(blocker);
    }
    let ids: Vec<String> = plan.items.into_iter().map(|item| item.id).collect();
    let input = serde_json::json!({
        "provider_id": provider_id,
        "model_id": model_id.unwrap_or_default(),
        "packs": ids,
    });
    // Runtime installs are not tied to a media item; media_id 0 by convention.
    state.0.submit(NewJob::new(INSTALL_JOB, 0).input(input))
}

#[tauri::command]
pub async fn runtime_doctor() -> Result<Vec<PackStatus>, String> {
    tauri::async_runtime::spawn_blocking(|| {
        doctor(root(), &builtin_catalog(), &env_lookup, &default_probe)
    })
    .await
    .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests;
