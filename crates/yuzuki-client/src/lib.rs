//! Client for the YUZUKI Desktop local control endpoint (docs/07-agent-control.md).
//!
//! Transport only: discovery, authentication and JSON envelopes. Every
//! behavior lives in the Desktop's Operation Registry; the CLI and the MCP
//! server reach it through this crate and add no business logic.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

/// Tauri identifier of the Desktop; its app data directory is named after it.
pub const APP_IDENTIFIER: &str = "com.avabbbb.tsubame";
pub const CONTROL_FILE: &str = "control.json";
/// Wire protocol this client speaks; must equal control.json protocol.
pub const PROTOCOL_VERSION: u32 = 1;
/// Explicit path to control.json, overriding the per-user default.
pub const CONTROL_FILE_ENV: &str = "YUZUKI_CONTROL_FILE";
/// Explicit Desktop executable to launch when it is not running.
pub const DESKTOP_EXE_ENV: &str = "YUZUKI_DESKTOP";

const HEALTH_TIMEOUT: Duration = Duration::from_secs(5);
const LAUNCH_POLL: Duration = Duration::from_millis(250);
/// Executable name the Desktop ships under (Tauri mainBinaryName), next to this sidecar.
/// Not YUZUKI or yuzuki: on case-insensitive filesystems those resolve to this CLI itself.
const DESKTOP_NAMES: &[&str] = &["tsubame"];

/// Contents of control.json, written by the Desktop at start.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlInfo {
    pub protocol: u32,
    pub addr: String,
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

/// A failure with a stable machine-readable code: the server's error code,
/// or one of the client codes desktop_not_running, launch_failed,
/// transport, protocol_mismatch, bad_response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientError {
    pub code: String,
    pub message: String,
    #[serde(default)]
    pub retryable: bool,
}

impl ClientError {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            message: message.into(),
            retryable: false,
        }
    }
}

impl std::fmt::Display for ClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ClientError {}

/// One entry of GET /v1/ops.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OpMeta {
    pub name: String,
    pub mutates: bool,
    pub expected_revision: bool,
    #[serde(default)]
    pub side_effects: Vec<Value>,
    #[serde(default)]
    pub cancellation: Value,
    /// JSON Schema of the operation's input.
    #[serde(default)]
    pub input_schema: Value,
}

#[derive(Debug, Clone)]
pub struct DiscoverOptions {
    /// control.json to read; default: YUZUKI_CONTROL_FILE, else the
    /// Desktop's per-user app data directory.
    pub control_file: Option<PathBuf>,
    /// Start the Desktop when no endpoint answers.
    pub launch: bool,
    /// Desktop executable; default: YUZUKI_DESKTOP, else next to this binary.
    pub desktop_exe: Option<PathBuf>,
    /// How long a launched Desktop has to bring its endpoint up.
    pub launch_timeout: Duration,
}

impl Default for DiscoverOptions {
    fn default() -> Self {
        Self {
            control_file: None,
            launch: false,
            desktop_exe: None,
            launch_timeout: Duration::from_secs(30),
        }
    }
}

/// A verified connection to a running Desktop.
pub struct ControlClient {
    info: ControlInfo,
    control_file: PathBuf,
}

impl std::fmt::Debug for ControlClient {
    // The token never reaches logs.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ControlClient")
            .field("addr", &self.info.addr)
            .field("pid", &self.info.pid)
            .field("control_file", &self.control_file)
            .finish()
    }
}

impl ControlClient {
    /// Reads control.json and checks /v1/health. With launch, starts the
    /// Desktop when nothing answers and waits for its endpoint; it never
    /// falls back to another install or port.
    pub fn discover(opts: &DiscoverOptions) -> Result<Self, ClientError> {
        let path = match &opts.control_file {
            Some(path) => path.clone(),
            None => default_control_file()?,
        };
        match Self::connect_file(&path) {
            Ok(client) => return Ok(client),
            Err(error) if !opts.launch || error.code != "desktop_not_running" => return Err(error),
            Err(_) => {}
        }
        let exe = desktop_exe(opts.desktop_exe.as_deref())?;
        let mut command = Command::new(&exe);
        // The Desktop must outlive this call and must not hold the caller's
        // stdout/stderr open (a pipe reader would never see EOF).
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        }
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            command.process_group(0);
        }
        command.spawn().map_err(|e| {
            ClientError::new(
                "launch_failed",
                format!("cannot start {}: {e}", exe.display()),
            )
        })?;
        let deadline = Instant::now() + opts.launch_timeout;
        loop {
            match Self::connect_file(&path) {
                Ok(client) => return Ok(client),
                Err(last) if Instant::now() >= deadline => {
                    return Err(ClientError::new(
                        "launch_failed",
                        format!(
                            "started {} but no endpoint answered within {}s: {}",
                            exe.display(),
                            opts.launch_timeout.as_secs(),
                            last.message
                        ),
                    ))
                }
                Err(_) => std::thread::sleep(LAUNCH_POLL),
            }
        }
    }

    fn connect_file(path: &Path) -> Result<Self, ClientError> {
        let text = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Err(ClientError::new(
                    "desktop_not_running",
                    format!("{} does not exist; start YUZUKI", path.display()),
                ))
            }
            Err(e) => {
                return Err(ClientError::new(
                    "transport",
                    format!("cannot read {}: {e}", path.display()),
                ))
            }
        };
        let info: ControlInfo = serde_json::from_str(&text).map_err(|e| {
            ClientError::new(
                "bad_response",
                format!("{} is not a control file: {e}", path.display()),
            )
        })?;
        if info.protocol != PROTOCOL_VERSION {
            return Err(ClientError::new(
                "protocol_mismatch",
                format!(
                    "Desktop speaks protocol {}, this client {PROTOCOL_VERSION}",
                    info.protocol
                ),
            ));
        }
        let client = Self {
            info,
            control_file: path.to_path_buf(),
        };
        match client.health() {
            Ok(_) => Ok(client),
            // A file left behind by a Desktop that has exited.
            Err(e) if e.code == "transport" => Err(ClientError::new(
                "desktop_not_running",
                format!("{} is stale ({})", path.display(), e.message),
            )),
            Err(e) => Err(e),
        }
    }

    pub fn addr(&self) -> &str {
        &self.info.addr
    }

    pub fn pid(&self) -> u32 {
        self.info.pid
    }

    pub fn control_file(&self) -> &Path {
        &self.control_file
    }

    /// GET /v1/health.
    pub fn health(&self) -> Result<Value, ClientError> {
        self.request("GET", "/v1/health", None, Some(HEALTH_TIMEOUT))
    }

    /// GET /v1/ops: the Desktop's Operation Registry.
    pub fn list_ops(&self) -> Result<Vec<OpMeta>, ClientError> {
        let ops = self.request("GET", "/v1/ops", None, Some(HEALTH_TIMEOUT))?;
        serde_json::from_value(ops)
            .map_err(|e| ClientError::new("bad_response", format!("GET /v1/ops: {e}")))
    }

    /// POST /v1/ops/<name>. No read timeout: job-backed operations such as
    /// segment.regenerate answer only when their job finishes.
    pub fn call(&self, name: &str, input: Value) -> Result<Value, ClientError> {
        self.request("POST", &format!("/v1/ops/{name}"), Some(&input), None)
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
        timeout: Option<Duration>,
    ) -> Result<Value, ClientError> {
        let addr = &self.info.addr;
        let transport = |e: std::io::Error| {
            ClientError::new("transport", format!("{method} {path} at {addr}: {e}"))
        };
        let mut stream = TcpStream::connect(addr.as_str()).map_err(transport)?;
        stream.set_read_timeout(timeout).map_err(transport)?;
        let body = body.map(Value::to_string).unwrap_or_default();
        let raw = format!(
            "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
            token = self.info.token,
            len = body.len()
        );
        stream.write_all(raw.as_bytes()).map_err(transport)?;
        let mut response = Vec::new();
        stream.read_to_end(&mut response).map_err(transport)?;
        envelope(parse_response(&response)?)
    }
}

/// control.json path: YUZUKI_CONTROL_FILE, else Tauri's app data directory
/// (<data dir>/com.avabbbb.tsubame/control.json).
pub fn default_control_file() -> Result<PathBuf, ClientError> {
    if let Some(path) = std::env::var_os(CONTROL_FILE_ENV) {
        return Ok(PathBuf::from(path));
    }
    data_dir()
        .map(|dir| dir.join(APP_IDENTIFIER).join(CONTROL_FILE))
        .ok_or_else(|| {
            ClientError::new(
                "desktop_not_running",
                "cannot locate the per-user data directory",
            )
        })
}

#[cfg(windows)]
fn data_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

#[cfg(target_os = "macos")]
fn data_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
}

#[cfg(all(unix, not(target_os = "macos")))]
fn data_dir() -> Option<PathBuf> {
    std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|dir| dir.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".local/share")))
}

fn desktop_exe(explicit: Option<&Path>) -> Result<PathBuf, ClientError> {
    if let Some(path) = explicit {
        return Ok(path.to_path_buf());
    }
    if let Some(path) = std::env::var_os(DESKTOP_EXE_ENV) {
        return Ok(PathBuf::from(path));
    }
    let me =
        std::env::current_exe().map_err(|e| ClientError::new("launch_failed", e.to_string()))?;
    let dir = me
        .parent()
        .ok_or_else(|| ClientError::new("launch_failed", "executable has no parent directory"))?;
    let tried: Vec<PathBuf> = DESKTOP_NAMES
        .iter()
        .map(|name| dir.join(format!("{name}{}", std::env::consts::EXE_SUFFIX)))
        .collect();
    tried
        .iter()
        .find(|path| path.is_file() && !is_same_file(path, &me))
        .cloned()
        .ok_or_else(|| {
            let tried: Vec<String> = tried.iter().map(|p| p.display().to_string()).collect();
            ClientError::new(
                "launch_failed",
                format!(
                    "no Desktop executable found (tried {}); set {DESKTOP_EXE_ENV}",
                    tried.join(", ")
                ),
            )
        })
}

/// True when both paths name the same file (case-insensitive filesystems, symlinks).
fn is_same_file(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

fn parse_response(bytes: &[u8]) -> Result<(u16, Value), ClientError> {
    let bad = |message: String| ClientError::new("bad_response", message);
    let split = bytes
        .windows(4)
        .position(|w| w == b"\r\n\r\n")
        .ok_or_else(|| bad("no HTTP header terminator".into()))?;
    let head =
        std::str::from_utf8(&bytes[..split]).map_err(|_| bad("non-UTF-8 HTTP header".into()))?;
    let status = head
        .split(' ')
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| bad(format!("no HTTP status in {head:?}")))?;
    let body = serde_json::from_slice(&bytes[split + 4..])
        .map_err(|e| bad(format!("HTTP {status} body is not JSON: {e}")))?;
    Ok((status, body))
}

/// {ok: true, result} -> result; {ok: false, error} -> the server's error.
fn envelope((status, body): (u16, Value)) -> Result<Value, ClientError> {
    match body.get("ok").and_then(Value::as_bool) {
        Some(true) => Ok(body.get("result").cloned().unwrap_or(Value::Null)),
        Some(false) => Err(
            serde_json::from_value(body["error"].clone()).unwrap_or_else(|_| {
                ClientError::new(
                    "bad_response",
                    format!("HTTP {status} with a malformed error: {body}"),
                )
            }),
        ),
        None => Err(ClientError::new(
            "bad_response",
            format!("HTTP {status} without an ok field"),
        )),
    }
}

#[cfg(test)]
mod tests;
