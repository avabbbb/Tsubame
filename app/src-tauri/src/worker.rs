//! Worker protocol v1 host (PR #10).
//!
//! Contract: `docs/19-dubbing-pipeline.md` § "Worker protocol v1". JSON Lines over
//! stdio; stdout carries protocol messages only, stderr carries free-form logs.
//!
//! ```text
//! worker → hello     host checks `protocol == 1` and the requested capability
//! host   → request   `input.out_dir` is set by the host (overwrites any caller value)
//! worker → progress | log      (streamed to the caller as `WorkerEvent`s)
//! worker → result | error      (ends the request)
//! host   → cancel    then `cancel_grace` (2 s) for an `error` reply, else kill
//! host   → shutdown  then `cancel_grace` for a clean exit, else kill
//! ```
//!
//! Host guarantees enforced here:
//! - **Handshake:** first stdout line must be `hello` within `hello_timeout`;
//!   `protocol` must equal [`PROTOCOL_VERSION`]; every required capability id must
//!   be announced. Failures kill the process.
//! - **Strict stdout:** a non-JSON line, an unknown message type or a message for
//!   another request id is a `worker_protocol_violation`; the worker is killed.
//! - **Input immutability:** sha256 of every `WorkerRequest::input_files` entry is
//!   taken before the request and re-checked when the request ends (result, error,
//!   exit, cancel or kill). A change fails with `input_mutated` (never retryable).
//! - **Output confinement:** reported output paths must resolve (after symlinks)
//!   inside `out_dir` and be existing regular files, else `output_escape` /
//!   `output_missing`. Reported paths are found by key convention: any object key
//!   `path` or ending in `_path` (string), or `paths` / ending in `_paths` (array
//!   of strings), at any depth of `output`. Relative paths resolve against
//!   `out_dir`. [`promote_output`] moves a validated file into the asset store
//!   atomically (rename; copy + rename across filesystems).
//!
//! The host never writes SQLite on behalf of a worker; callers (normally a
//! [`crate::jobs::JobHandle`]) persist progress and results.
//!
//! Limitation: kill targets the direct child process only (no process-group kill
//! yet). Workers that spawn their own subprocesses must clean them up on cancel.

// Public API consumed by the other PR #10 tracks; unused until they land.
#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use std::collections::VecDeque;
use std::fs;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

pub const PROTOCOL_VERSION: u32 = 1;
pub const DEFAULT_HELLO_TIMEOUT: Duration = Duration::from_secs(20);
pub const CANCEL_GRACE: Duration = Duration::from_secs(2);
const POLL: Duration = Duration::from_millis(20);
const STDERR_TAIL_LINES: usize = 20;

/// Error codes produced by the host itself. Worker-reported codes pass through.
pub mod codes {
    pub const SPAWN_FAILED: &str = "worker_spawn_failed";
    pub const HELLO_TIMEOUT: &str = "worker_hello_timeout";
    pub const PROTOCOL_MISMATCH: &str = "worker_protocol_mismatch";
    pub const MISSING_CAPABILITY: &str = "worker_missing_capability";
    pub const PROTOCOL_VIOLATION: &str = "worker_protocol_violation";
    pub const WORKER_EXITED: &str = "worker_exited";
    pub const WORKER_DEAD: &str = "worker_dead";
    pub const TIMEOUT: &str = "worker_timeout";
    pub const CANCELLED: &str = "cancelled";
    pub const INPUT_MUTATED: &str = "input_mutated";
    pub const INPUT_MISSING: &str = "input_missing";
    pub const OUTPUT_ESCAPE: &str = "output_escape";
    pub const OUTPUT_MISSING: &str = "output_missing";
    pub const BAD_REQUEST: &str = "bad_request";
    pub const IO: &str = "worker_io";
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkerError {
    pub code: String,
    pub retryable: bool,
    pub message: String,
}

impl WorkerError {
    pub fn new(code: &str, retryable: bool, message: impl Into<String>) -> Self {
        Self {
            code: code.to_string(),
            retryable,
            message: message.into(),
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.code == codes::CANCELLED
    }
}

impl std::fmt::Display for WorkerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for WorkerError {}

/// How to launch a worker process.
#[derive(Debug, Clone)]
pub struct WorkerSpec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub current_dir: Option<PathBuf>,
    pub hello_timeout: Duration,
    /// Grace period after `cancel` / `shutdown` before the process is killed.
    pub cancel_grace: Duration,
    /// Optional wall-clock limit for one request; exceeded → kill + `worker_timeout`.
    pub request_timeout: Option<Duration>,
}

impl WorkerSpec {
    pub fn new(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: Vec::new(),
            current_dir: None,
            hello_timeout: DEFAULT_HELLO_TIMEOUT,
            cancel_grace: CANCEL_GRACE,
            request_timeout: None,
        }
    }

    /// `python -u <script> [args…]` with `PYTHONUNBUFFERED=1`.
    pub fn python(python: impl Into<PathBuf>, script: impl AsRef<Path>) -> Self {
        let mut spec = Self::new(python);
        spec.args = vec![
            "-u".to_string(),
            script.as_ref().to_string_lossy().into_owned(),
        ];
        spec.env
            .push(("PYTHONUNBUFFERED".to_string(), "1".to_string()));
        spec
    }

    pub fn arg(mut self, value: impl Into<String>) -> Self {
        self.args.push(value.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerCapability {
    pub id: String,
    #[serde(default)]
    pub features: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkerHello {
    pub protocol: u32,
    pub worker: String,
    pub version: String,
    #[serde(default)]
    pub capabilities: Vec<WorkerCapability>,
    #[serde(default)]
    pub devices: Vec<String>,
}

impl WorkerHello {
    pub fn has_capability(&self, id: &str) -> bool {
        self.capabilities.iter().any(|cap| cap.id == id)
    }
}

/// One request. `input` must be a JSON object; the host inserts `out_dir`.
#[derive(Debug, Clone)]
pub struct WorkerRequest {
    pub id: String,
    pub op: String,
    pub input: Value,
    /// Files the worker reads; hashed before and after (immutability check).
    pub input_files: Vec<PathBuf>,
    /// The only directory the worker may write into. Created if missing.
    pub out_dir: PathBuf,
}

#[derive(Debug, Clone, PartialEq)]
pub enum WorkerEvent {
    Progress {
        stage: String,
        done: u64,
        total: u64,
    },
    Log {
        level: String,
        message: String,
    },
    /// A raw stderr line (free-form worker log).
    Stderr(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct WorkerOutput {
    pub output: Value,
    /// Canonical paths of reported output files, all inside `out_dir`.
    pub output_files: Vec<PathBuf>,
}

enum Incoming {
    Stdout(String),
    StdoutClosed,
    Stderr(String),
}

/// A spawned, handshaken worker process. Serves requests one at a time.
pub struct WorkerSession {
    child: Child,
    stdin: Option<ChildStdin>,
    rx: Receiver<Incoming>,
    hello: WorkerHello,
    spec: WorkerSpec,
    stderr_tail: Arc<Mutex<VecDeque<String>>>,
    dead: bool,
}

impl WorkerSession {
    /// Spawns the worker, reads `hello` and checks protocol + capabilities.
    pub fn spawn(spec: &WorkerSpec, required_capabilities: &[&str]) -> Result<Self, WorkerError> {
        let mut command = Command::new(&spec.program);
        command
            .args(&spec.args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        if let Some(dir) = &spec.current_dir {
            command.current_dir(dir);
        }
        let mut child = command.spawn().map_err(|e| {
            WorkerError::new(
                codes::SPAWN_FAILED,
                false,
                format!("failed to start worker {}: {e}", spec.program.display()),
            )
        })?;

        let (tx, rx) = mpsc::channel();
        let stdout = child.stdout.take().expect("piped stdout");
        let stderr = child.stderr.take().expect("piped stderr");
        let stdin = child.stdin.take();
        let stderr_tail = Arc::new(Mutex::new(VecDeque::new()));

        let out_tx = tx.clone();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let trimmed = line.trim_end_matches(['\r', '\n']).to_string();
                        if out_tx.send(Incoming::Stdout(trimmed)).is_err() {
                            return;
                        }
                    }
                }
            }
            let _ = out_tx.send(Incoming::StdoutClosed);
        });
        let tail = Arc::clone(&stderr_tail);
        thread::spawn(move || {
            let mut reader = BufReader::new(stderr);
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match reader.by_ref().read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf)
                            .trim_end_matches(['\r', '\n'])
                            .to_string();
                        if let Ok(mut tail) = tail.lock() {
                            if tail.len() == STDERR_TAIL_LINES {
                                tail.pop_front();
                            }
                            tail.push_back(line.clone());
                        }
                        if tx.send(Incoming::Stderr(line)).is_err() {
                            return;
                        }
                    }
                }
            }
        });

        let mut session = Self {
            child,
            stdin,
            rx,
            hello: WorkerHello {
                protocol: 0,
                worker: String::new(),
                version: String::new(),
                capabilities: Vec::new(),
                devices: Vec::new(),
            },
            spec: spec.clone(),
            stderr_tail,
            dead: false,
        };
        match session.read_hello(required_capabilities) {
            Ok(hello) => {
                session.hello = hello;
                Ok(session)
            }
            Err(error) => {
                session.kill();
                Err(error)
            }
        }
    }

    fn read_hello(&mut self, required: &[&str]) -> Result<WorkerHello, WorkerError> {
        let deadline = Instant::now() + self.spec.hello_timeout;
        let line = loop {
            let now = Instant::now();
            if now >= deadline {
                return Err(WorkerError::new(
                    codes::HELLO_TIMEOUT,
                    true,
                    format!(
                        "worker sent no hello within {} ms",
                        self.spec.hello_timeout.as_millis()
                    ),
                ));
            }
            match self.rx.recv_timeout(deadline - now) {
                Ok(Incoming::Stdout(line)) if line.trim().is_empty() => continue,
                Ok(Incoming::Stdout(line)) => break line,
                Ok(Incoming::Stderr(_)) => continue,
                Ok(Incoming::StdoutClosed) | Err(RecvTimeoutError::Disconnected) => {
                    return Err(self.exited_error("worker exited before hello"));
                }
                Err(RecvTimeoutError::Timeout) => continue,
            }
        };
        let value: Value = serde_json::from_str(&line).map_err(|e| {
            WorkerError::new(
                codes::PROTOCOL_VIOLATION,
                false,
                format!("first stdout line is not JSON ({e}): {}", clip(&line)),
            )
        })?;
        if value.get("type").and_then(Value::as_str) != Some("hello") {
            return Err(WorkerError::new(
                codes::PROTOCOL_VIOLATION,
                false,
                format!("expected hello, got: {}", clip(&line)),
            ));
        }
        let protocol = value.get("protocol").and_then(Value::as_u64);
        if protocol != Some(u64::from(PROTOCOL_VERSION)) {
            return Err(WorkerError::new(
                codes::PROTOCOL_MISMATCH,
                false,
                format!(
                    "worker speaks protocol {}, host requires {PROTOCOL_VERSION}",
                    protocol.map_or_else(|| "?".to_string(), |v| v.to_string())
                ),
            ));
        }
        let hello: WorkerHello = serde_json::from_value(value).map_err(|e| {
            WorkerError::new(
                codes::PROTOCOL_VIOLATION,
                false,
                format!("malformed hello: {e}"),
            )
        })?;
        let missing = required
            .iter()
            .filter(|id| !hello.has_capability(id))
            .copied()
            .collect::<Vec<_>>();
        if !missing.is_empty() {
            return Err(WorkerError::new(
                codes::MISSING_CAPABILITY,
                false,
                format!(
                    "worker {} {} does not provide {}",
                    hello.worker,
                    hello.version,
                    missing.join(", ")
                ),
            ));
        }
        Ok(hello)
    }

    pub fn hello(&self) -> &WorkerHello {
        &self.hello
    }

    /// True once the process was killed or broke the protocol; no further requests.
    pub fn is_dead(&self) -> bool {
        self.dead
    }

    /// Runs one request to completion, streaming progress/log/stderr to `on_event`.
    /// `cancel` is polled; once set the host sends `cancel` and kills the worker
    /// if no `error` arrives within `cancel_grace`.
    pub fn run(
        &mut self,
        request: &WorkerRequest,
        cancel: &AtomicBool,
        on_event: &mut dyn FnMut(WorkerEvent),
    ) -> Result<WorkerOutput, WorkerError> {
        if self.dead {
            return Err(WorkerError::new(
                codes::WORKER_DEAD,
                true,
                "worker session is no longer usable",
            ));
        }
        if !self.hello.has_capability(&request.op) {
            return Err(WorkerError::new(
                codes::MISSING_CAPABILITY,
                false,
                format!(
                    "worker {} does not provide {}",
                    self.hello.worker, request.op
                ),
            ));
        }
        if request.id.trim().is_empty() {
            return Err(WorkerError::new(
                codes::BAD_REQUEST,
                false,
                "request id is empty",
            ));
        }

        fs::create_dir_all(&request.out_dir).map_err(|e| {
            WorkerError::new(codes::IO, true, format!("cannot create out_dir: {e}"))
        })?;
        let out_dir = request.out_dir.canonicalize().map_err(|e| {
            WorkerError::new(codes::IO, true, format!("cannot resolve out_dir: {e}"))
        })?;
        let mut input = match &request.input {
            Value::Object(map) => map.clone(),
            Value::Null => Map::new(),
            _ => {
                return Err(WorkerError::new(
                    codes::BAD_REQUEST,
                    false,
                    "request input must be a JSON object",
                ))
            }
        };
        input.insert(
            "out_dir".to_string(),
            Value::String(out_dir.to_string_lossy().into_owned()),
        );

        let before = hash_inputs(&request.input_files)?;

        // Drain stderr that arrived between requests.
        while let Ok(message) = self.rx.try_recv() {
            match message {
                Incoming::Stderr(line) => on_event(WorkerEvent::Stderr(line)),
                Incoming::StdoutClosed => {
                    self.dead = true;
                    return Err(self.exited_error("worker exited between requests"));
                }
                Incoming::Stdout(line) => {
                    self.kill();
                    return Err(WorkerError::new(
                        codes::PROTOCOL_VIOLATION,
                        false,
                        format!("unsolicited stdout line: {}", clip(&line)),
                    ));
                }
            }
        }

        let line = serde_json::json!({
            "type": "request",
            "id": request.id,
            "op": request.op,
            "input": Value::Object(input),
        });
        if let Err(error) = self.send(&line) {
            self.kill();
            return Err(error);
        }

        let outcome = self.pump(request, &out_dir, cancel, on_event);
        // Input immutability is checked whatever the outcome; it wins over it.
        match verify_inputs(&request.input_files, &before) {
            Err(mutated) => Err(mutated),
            Ok(()) => outcome,
        }
    }

    fn pump(
        &mut self,
        request: &WorkerRequest,
        out_dir: &Path,
        cancel: &AtomicBool,
        on_event: &mut dyn FnMut(WorkerEvent),
    ) -> Result<WorkerOutput, WorkerError> {
        let started = Instant::now();
        let mut cancel_deadline: Option<Instant> = None;
        loop {
            if cancel_deadline.is_none() && cancel.load(Ordering::SeqCst) {
                let message = serde_json::json!({"type": "cancel", "id": request.id});
                if self.send(&message).is_err() {
                    self.kill();
                    return Err(cancelled("worker stdin closed while cancelling; killed"));
                }
                cancel_deadline = Some(Instant::now() + self.spec.cancel_grace);
            }
            if let Some(deadline) = cancel_deadline {
                if Instant::now() >= deadline {
                    self.kill();
                    return Err(cancelled(format!(
                        "worker did not acknowledge cancel within {} ms; killed",
                        self.spec.cancel_grace.as_millis()
                    )));
                }
            }
            if let Some(limit) = self.spec.request_timeout {
                if cancel_deadline.is_none() && started.elapsed() >= limit {
                    self.kill();
                    return Err(WorkerError::new(
                        codes::TIMEOUT,
                        true,
                        format!("request exceeded {} ms; killed", limit.as_millis()),
                    ));
                }
            }

            let line = match self.rx.recv_timeout(POLL) {
                Err(RecvTimeoutError::Timeout) => continue,
                Ok(Incoming::Stderr(line)) => {
                    on_event(WorkerEvent::Stderr(line));
                    continue;
                }
                Ok(Incoming::StdoutClosed) | Err(RecvTimeoutError::Disconnected) => {
                    self.dead = true;
                    let _ = self.child.wait();
                    if cancel_deadline.is_some() {
                        return Err(cancelled("worker exited after cancel"));
                    }
                    return Err(self.exited_error("worker exited mid-request"));
                }
                Ok(Incoming::Stdout(line)) => line,
            };
            if line.trim().is_empty() {
                continue;
            }
            let message: Value = match serde_json::from_str(&line) {
                Ok(value) => value,
                Err(_) => return Err(self.violation(format!("non-JSON stdout: {}", clip(&line)))),
            };
            let kind = message.get("type").and_then(Value::as_str).unwrap_or("");
            let id = message.get("id").and_then(Value::as_str).unwrap_or("");
            if id != request.id {
                return Err(self.violation(format!("{kind} message for unknown request id {id:?}")));
            }
            match kind {
                "progress" => on_event(WorkerEvent::Progress {
                    stage: str_field(&message, "stage"),
                    done: message.get("done").and_then(Value::as_u64).unwrap_or(0),
                    total: message.get("total").and_then(Value::as_u64).unwrap_or(0),
                }),
                "log" => on_event(WorkerEvent::Log {
                    level: match message.get("level").and_then(Value::as_str) {
                        Some(level @ ("info" | "warn" | "error")) => level.to_string(),
                        _ => "info".to_string(),
                    },
                    message: str_field(&message, "message"),
                }),
                "result" => {
                    let output = message.get("output").cloned().unwrap_or(Value::Null);
                    let output_files = validate_output_paths(&output, out_dir)?;
                    return Ok(WorkerOutput {
                        output,
                        output_files,
                    });
                }
                "error" => {
                    let code = message
                        .get("code")
                        .and_then(Value::as_str)
                        .filter(|code| !code.is_empty())
                        .unwrap_or("worker_error");
                    return Err(WorkerError::new(
                        code,
                        message
                            .get("retryable")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                        str_field(&message, "message"),
                    ));
                }
                other => {
                    return Err(self.violation(format!("unknown message type {other:?}")));
                }
            }
        }
    }

    /// Sends `shutdown`, waits `cancel_grace` for exit, then kills.
    pub fn shutdown(mut self) {
        self.shutdown_inner(None);
    }

    /// Like [`Self::shutdown`], but forwards the worker's remaining stderr lines
    /// (stderr is a separate pipe and may trail the final protocol message).
    pub fn shutdown_with(mut self, on_event: &mut dyn FnMut(WorkerEvent)) {
        self.shutdown_inner(Some(on_event));
    }

    fn shutdown_inner(&mut self, mut on_event: Option<&mut dyn FnMut(WorkerEvent)>) {
        if !self.dead {
            let _ = self.send(&serde_json::json!({"type": "shutdown"}));
        }
        self.stdin.take();
        let deadline = Instant::now() + self.spec.cancel_grace;
        let mut exited = false;
        while Instant::now() < deadline {
            if !exited {
                match self.child.try_wait() {
                    Ok(Some(_)) | Err(_) => {
                        exited = true;
                        self.dead = true;
                        if on_event.is_none() {
                            return;
                        }
                    }
                    Ok(None) => {}
                }
            }
            match self.rx.recv_timeout(POLL) {
                Ok(Incoming::Stderr(line)) => {
                    if let Some(callback) = on_event.as_mut() {
                        callback(WorkerEvent::Stderr(line));
                    }
                }
                Ok(_) | Err(RecvTimeoutError::Timeout) => {}
                // Both pipes closed: everything the worker wrote has been seen.
                Err(RecvTimeoutError::Disconnected) if exited => return,
                Err(RecvTimeoutError::Disconnected) => thread::sleep(POLL),
            }
        }
        if !exited {
            self.kill();
        }
    }

    fn send(&mut self, message: &Value) -> Result<(), WorkerError> {
        let stdin = self
            .stdin
            .as_mut()
            .ok_or_else(|| WorkerError::new(codes::WORKER_DEAD, true, "worker stdin closed"))?;
        let mut line = serde_json::to_string(message)
            .map_err(|e| WorkerError::new(codes::BAD_REQUEST, false, e.to_string()))?;
        line.push('\n');
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.flush())
            .map_err(|e| WorkerError::new(codes::WORKER_DEAD, true, format!("write failed: {e}")))
    }

    fn violation(&mut self, message: String) -> WorkerError {
        self.kill();
        WorkerError::new(codes::PROTOCOL_VIOLATION, false, message)
    }

    fn exited_error(&mut self, context: &str) -> WorkerError {
        // Give the stderr reader a moment to collect the last lines.
        thread::sleep(Duration::from_millis(50));
        let status = self
            .child
            .try_wait()
            .ok()
            .flatten()
            .map(|status| status.to_string())
            .unwrap_or_else(|| "still running".to_string());
        let tail = self
            .stderr_tail
            .lock()
            .map(|tail| tail.iter().cloned().collect::<Vec<_>>().join("\n"))
            .unwrap_or_default();
        let mut message = format!("{context} ({status})");
        if !tail.is_empty() {
            message.push_str(": ");
            message.push_str(&tail);
        }
        WorkerError::new(codes::WORKER_EXITED, true, message)
    }

    fn kill(&mut self) {
        self.dead = true;
        self.stdin.take();
        let _ = self.child.kill();
        let _ = self.child.wait();
    }

    pub fn stderr_tail(&self) -> Vec<String> {
        self.stderr_tail
            .lock()
            .map(|tail| tail.iter().cloned().collect())
            .unwrap_or_default()
    }
}

impl Drop for WorkerSession {
    fn drop(&mut self) {
        if !self.dead {
            self.shutdown_inner(None);
        }
    }
}

/// Spawn → handshake (requiring `request.op`) → run one request → shutdown.
pub fn run_worker(
    spec: &WorkerSpec,
    request: &WorkerRequest,
    cancel: &AtomicBool,
    on_event: &mut dyn FnMut(WorkerEvent),
) -> Result<WorkerOutput, WorkerError> {
    if cancel.load(Ordering::SeqCst) {
        return Err(cancelled("cancelled before the worker started"));
    }
    let mut session = WorkerSession::spawn(spec, &[request.op.as_str()])?;
    let result = session.run(request, cancel, on_event);
    session.shutdown_with(on_event);
    result
}

fn cancelled(message: impl Into<String>) -> WorkerError {
    WorkerError::new(codes::CANCELLED, true, message)
}

fn str_field(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn clip(line: &str) -> String {
    line.chars().take(200).collect()
}

/// Lower-case hex sha256 of a file's contents.
pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buf)?;
        if read == 0 {
            break;
        }
        hasher.update(&buf[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn hash_inputs(files: &[PathBuf]) -> Result<Vec<String>, WorkerError> {
    files
        .iter()
        .map(|path| {
            sha256_file(path).map_err(|e| {
                WorkerError::new(
                    codes::INPUT_MISSING,
                    false,
                    format!("cannot read input {}: {e}", path.display()),
                )
            })
        })
        .collect()
}

fn verify_inputs(files: &[PathBuf], before: &[String]) -> Result<(), WorkerError> {
    for (path, expected) in files.iter().zip(before) {
        let now = sha256_file(path).unwrap_or_else(|_| "<unreadable>".to_string());
        if &now != expected {
            return Err(WorkerError::new(
                codes::INPUT_MUTATED,
                false,
                format!("input file changed during the request: {}", path.display()),
            ));
        }
    }
    Ok(())
}

fn is_path_key(key: &str) -> bool {
    key == "path" || key.ends_with("_path")
}

fn is_paths_key(key: &str) -> bool {
    key == "paths" || key.ends_with("_paths")
}

fn collect_paths(value: &Value, found: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (key, item) in map {
                match item {
                    Value::String(path) if is_path_key(key) => found.push(path.clone()),
                    Value::Array(items) if is_paths_key(key) => {
                        for entry in items {
                            match entry {
                                Value::String(path) => found.push(path.clone()),
                                other => collect_paths(other, found),
                            }
                        }
                    }
                    other => collect_paths(other, found),
                }
            }
        }
        Value::Array(items) => items.iter().for_each(|item| collect_paths(item, found)),
        _ => {}
    }
}

/// Validates every reported output path (see module docs) against `out_dir`
/// (which must already be canonical). Returns canonical file paths.
pub fn validate_output_paths(output: &Value, out_dir: &Path) -> Result<Vec<PathBuf>, WorkerError> {
    let mut reported = Vec::new();
    collect_paths(output, &mut reported);
    let mut files = Vec::new();
    for raw in reported {
        let candidate = Path::new(&raw);
        let joined = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            out_dir.join(candidate)
        };
        let resolved = match joined.canonicalize() {
            Ok(path) => path,
            Err(_) => {
                // A non-existent path that lexically escapes is still an escape.
                let lexical_escape = candidate.is_absolute() && !candidate.starts_with(out_dir)
                    || candidate
                        .components()
                        .any(|c| matches!(c, std::path::Component::ParentDir));
                let code = if lexical_escape {
                    codes::OUTPUT_ESCAPE
                } else {
                    codes::OUTPUT_MISSING
                };
                return Err(WorkerError::new(
                    code,
                    false,
                    format!("reported output does not exist inside out_dir: {raw}"),
                ));
            }
        };
        if !resolved.starts_with(out_dir) {
            return Err(WorkerError::new(
                codes::OUTPUT_ESCAPE,
                false,
                format!("reported output escapes out_dir: {raw}"),
            ));
        }
        if !resolved.is_file() {
            return Err(WorkerError::new(
                codes::OUTPUT_MISSING,
                false,
                format!("reported output is not a regular file: {raw}"),
            ));
        }
        if !files.contains(&resolved) {
            files.push(resolved);
        }
    }
    Ok(files)
}

/// Atomically moves a validated worker output to `dest` (asset store path):
/// rename when on the same filesystem, else copy to a temp file next to `dest`
/// and rename it into place.
pub fn promote_output(src: &Path, dest: &Path) -> io::Result<()> {
    if let Some(parent) = dest.parent() {
        fs::create_dir_all(parent)?;
    }
    if fs::rename(src, dest).is_ok() {
        return Ok(());
    }
    let tmp = dest.with_extension(format!("partial-{}", uuid::Uuid::new_v4()));
    fs::copy(src, &tmp)?;
    if let Err(error) = fs::rename(&tmp, dest) {
        let _ = fs::remove_file(&tmp);
        return Err(error);
    }
    let _ = fs::remove_file(src);
    Ok(())
}

#[cfg(test)]
pub(crate) mod test_support {
    use super::*;

    pub fn python() -> Option<String> {
        let python = std::env::var("TSUBAME_TEST_PYTHON").unwrap_or_else(|_| "python3".into());
        Command::new(&python)
            .arg("--version")
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|_| python)
    }

    pub fn echo_script() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../workers/dubber/echo_worker.py")
    }

    pub fn echo_spec(extra: &[&str]) -> Option<WorkerSpec> {
        let python = python()?;
        let mut spec = WorkerSpec::python(python, echo_script());
        spec.hello_timeout = Duration::from_secs(10);
        for arg in extra {
            spec = spec.arg(*arg);
        }
        Some(spec)
    }

    pub fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tsubame-{tag}-{}", uuid::Uuid::new_v4()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::*;
    use super::*;
    use serde_json::json;

    macro_rules! spec_or_skip {
        ($extra:expr) => {
            match echo_spec($extra) {
                Some(spec) => spec,
                None => {
                    eprintln!("python3 not available; skipping worker protocol test");
                    return;
                }
            }
        };
    }

    fn request(input: Value, out_dir: &Path) -> WorkerRequest {
        WorkerRequest {
            id: "job-1".into(),
            op: "test.echo".into(),
            input,
            input_files: Vec::new(),
            out_dir: out_dir.to_path_buf(),
        }
    }

    fn run(
        spec: &WorkerSpec,
        req: &WorkerRequest,
    ) -> (Result<WorkerOutput, WorkerError>, Vec<WorkerEvent>) {
        let cancel = AtomicBool::new(false);
        let mut events = Vec::new();
        let result = run_worker(spec, req, &cancel, &mut |event| events.push(event));
        (result, events)
    }

    #[test]
    fn handshake_success_reports_hello() {
        let spec = spec_or_skip!(&[]);
        let session = WorkerSession::spawn(&spec, &["test.echo"]).expect("handshake");
        assert_eq!(session.hello().protocol, 1);
        assert_eq!(session.hello().worker, "tsubame-echo");
        assert!(session.hello().has_capability("test.echo"));
        assert_eq!(session.hello().devices, vec!["cpu".to_string()]);
        session.shutdown();
    }

    #[test]
    fn protocol_mismatch_is_rejected() {
        let spec = spec_or_skip!(&["--protocol", "2"]);
        let error = WorkerSession::spawn(&spec, &["test.echo"]).err().unwrap();
        assert_eq!(error.code, codes::PROTOCOL_MISMATCH);
        assert!(!error.retryable);
    }

    #[test]
    fn missing_capability_is_rejected() {
        let spec = spec_or_skip!(&["--capability", "audio.mix"]);
        let error = WorkerSession::spawn(&spec, &["audio.spatialize"])
            .err()
            .unwrap();
        assert_eq!(error.code, codes::MISSING_CAPABILITY);
        assert!(error.message.contains("audio.spatialize"));
    }

    #[test]
    fn hello_timeout_kills_silent_worker() {
        let mut spec = spec_or_skip!(&["--no-hello"]);
        spec.hello_timeout = Duration::from_millis(400);
        let started = Instant::now();
        let error = WorkerSession::spawn(&spec, &["test.echo"]).err().unwrap();
        assert_eq!(error.code, codes::HELLO_TIMEOUT);
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn non_json_hello_is_a_protocol_violation() {
        // A shell "worker" whose first stdout line is not JSON.
        let mut spec = WorkerSpec::new("sh");
        spec.args = vec!["-c".into(), "echo not-json; sleep 5".into()];
        let error = WorkerSession::spawn(&spec, &["x"]).err().unwrap();
        assert_eq!(error.code, codes::PROTOCOL_VIOLATION);
    }

    #[test]
    fn spawn_failure_is_reported() {
        let spec = WorkerSpec::new("/nonexistent/tsubame-worker");
        let error = WorkerSession::spawn(&spec, &[]).err().unwrap();
        assert_eq!(error.code, codes::SPAWN_FAILED);
    }

    #[test]
    fn echo_result_carries_host_out_dir() {
        let spec = spec_or_skip!(&[]);
        let dir = temp_dir("echo");
        let (result, _) = run(
            &spec,
            &request(json!({"mode": "echo", "out_dir": "/elsewhere"}), &dir),
        );
        let output = result.expect("result").output;
        let reported = output["echo"]["out_dir"].as_str().unwrap();
        assert_eq!(Path::new(reported), dir.canonicalize().unwrap());
    }

    #[test]
    fn progress_and_logs_are_streamed() {
        let spec = spec_or_skip!(&[]);
        let dir = temp_dir("progress");
        let (result, events) = run(
            &spec,
            &request(json!({"mode": "progress", "steps": 4}), &dir),
        );
        assert_eq!(result.unwrap().output, json!({"steps": 4}));
        let progress = events
            .iter()
            .filter_map(|event| match event {
                WorkerEvent::Progress { done, total, stage } => {
                    Some((*done, *total, stage.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            progress,
            (1..=4)
                .map(|i| (i, 4, "echo".to_string()))
                .collect::<Vec<_>>()
        );

        let (result, events) = run(&spec, &request(json!({"mode": "log"}), &dir));
        assert!(result.is_ok());
        assert!(events.contains(&WorkerEvent::Log {
            level: "info".into(),
            message: "hello from the echo worker".into()
        }));
        assert!(events
            .iter()
            .any(|e| matches!(e, WorkerEvent::Stderr(line) if line == "raw stderr line")));
    }

    #[test]
    fn stray_print_does_not_break_the_protocol() {
        let spec = spec_or_skip!(&[]);
        let dir = temp_dir("noise");
        let (result, _) = run(&spec, &request(json!({"mode": "stdout_noise"}), &dir));
        assert_eq!(result.unwrap().output, json!({"noise": true}));
    }

    #[test]
    fn worker_error_passes_through() {
        let spec = spec_or_skip!(&[]);
        let dir = temp_dir("error");
        let (result, _) = run(
            &spec,
            &request(
                json!({"mode": "error", "code": "rate_limited", "retryable": true, "message": "slow down"}),
                &dir,
            ),
        );
        assert_eq!(
            result.unwrap_err(),
            WorkerError::new("rate_limited", true, "slow down")
        );
    }

    #[test]
    fn crash_is_reported_with_stderr_tail() {
        let spec = spec_or_skip!(&[]);
        let dir = temp_dir("crash");
        let (result, _) = run(&spec, &request(json!({"mode": "crash"}), &dir));
        let error = result.unwrap_err();
        assert_eq!(error.code, codes::WORKER_EXITED);
        assert!(error.retryable);
    }

    #[test]
    fn cooperative_cancel_returns_cancelled_quickly() {
        let spec = spec_or_skip!(&[]);
        let dir = temp_dir("cancel");
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        let started = Instant::now();
        let mut first_progress = true;
        let result = run_worker(
            &spec,
            &request(json!({"mode": "slow", "steps": 1000}), &dir),
            &cancel,
            &mut |event| {
                if matches!(event, WorkerEvent::Progress { .. }) && first_progress {
                    first_progress = false;
                    flag.store(true, Ordering::SeqCst);
                }
            },
        );
        let error = result.unwrap_err();
        assert_eq!(error.code, codes::CANCELLED);
        assert!(!error.message.contains("killed"), "{}", error.message);
        assert!(started.elapsed() < Duration::from_secs(10));
    }

    #[test]
    fn ignored_cancel_is_killed_after_grace() {
        let mut spec = spec_or_skip!(&[]);
        spec.cancel_grace = Duration::from_millis(300);
        let dir = temp_dir("kill");
        let mut session = WorkerSession::spawn(&spec, &["test.echo"]).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&cancel);
        thread::spawn(move || {
            thread::sleep(Duration::from_millis(200));
            flag.store(true, Ordering::SeqCst);
        });
        let started = Instant::now();
        let error = session
            .run(
                &request(json!({"mode": "ignore_cancel", "sleep_s": 30}), &dir),
                &cancel,
                &mut |_| {},
            )
            .unwrap_err();
        assert_eq!(error.code, codes::CANCELLED);
        assert!(error.message.contains("killed"));
        assert!(started.elapsed() < Duration::from_secs(5));
        assert!(session.is_dead());
        assert!(
            session.child.try_wait().unwrap().is_some(),
            "process reaped"
        );
    }

    #[test]
    fn request_timeout_kills_worker() {
        let mut spec = spec_or_skip!(&[]);
        spec.request_timeout = Some(Duration::from_millis(300));
        let dir = temp_dir("timeout");
        let (result, _) = run(
            &spec,
            &request(json!({"mode": "ignore_cancel", "sleep_s": 30}), &dir),
        );
        let error = result.unwrap_err();
        assert_eq!(error.code, codes::TIMEOUT);
        assert!(error.retryable);
    }

    #[test]
    fn input_mutation_fails_the_request() {
        let spec = spec_or_skip!(&[]);
        let dir = temp_dir("mutate");
        let input_path = dir.join("input.wav");
        fs::write(&input_path, b"RIFF").unwrap();
        let mut req = request(
            json!({"mode": "mutate_input", "input_path": input_path.to_string_lossy()}),
            &dir.join("out"),
        );
        req.input_files = vec![input_path.clone()];
        let (result, _) = run(&spec, &req);
        let error = result.unwrap_err();
        assert_eq!(error.code, codes::INPUT_MUTATED);
        assert!(!error.retryable);

        // An untouched input passes the same check.
        let mut req = request(json!({"mode": "echo"}), &dir.join("out"));
        req.input_files = vec![input_path];
        assert!(run(&spec, &req).0.is_ok());
    }

    #[test]
    fn missing_input_is_rejected_before_the_request() {
        let spec = spec_or_skip!(&[]);
        let dir = temp_dir("missing-input");
        let mut req = request(json!({"mode": "echo"}), &dir);
        req.input_files = vec![dir.join("nope.wav")];
        assert_eq!(run(&spec, &req).0.unwrap_err().code, codes::INPUT_MISSING);
    }

    #[test]
    fn outputs_inside_out_dir_are_accepted() {
        let spec = spec_or_skip!(&[]);
        let dir = temp_dir("write");
        let (result, _) = run(
            &spec,
            &request(json!({"mode": "write", "name": "clip.wav"}), &dir),
        );
        let output = result.unwrap();
        assert_eq!(
            output.output_files,
            vec![dir.canonicalize().unwrap().join("clip.wav")]
        );

        let store = dir.join("store/asset.wav");
        promote_output(&output.output_files[0], &store).unwrap();
        assert_eq!(fs::read_to_string(&store).unwrap(), "ok");
        assert!(!output.output_files[0].exists());
    }

    #[test]
    fn out_dir_escape_is_rejected() {
        let spec = spec_or_skip!(&[]);
        let root = temp_dir("escape");
        let outside = root.join("outside.wav");
        fs::write(&outside, b"x").unwrap();
        let out_dir = root.join("out");

        for escape in [
            outside.to_string_lossy().into_owned(),
            "../outside.wav".to_string(),
            "../does-not-exist.wav".to_string(),
        ] {
            let (result, _) = run(
                &spec,
                &request(json!({"mode": "escape", "escape_path": escape}), &out_dir),
            );
            let error = result.unwrap_err();
            assert_eq!(error.code, codes::OUTPUT_ESCAPE, "{escape}");
        }

        #[cfg(unix)]
        {
            // A symlink inside out_dir pointing outside is still an escape.
            std::os::unix::fs::symlink(&outside, out_dir.join("link.wav")).unwrap();
            let (result, _) = run(
                &spec,
                &request(
                    json!({"mode": "escape", "escape_path": "link.wav"}),
                    &out_dir,
                ),
            );
            assert_eq!(result.unwrap_err().code, codes::OUTPUT_ESCAPE);
        }
    }

    #[test]
    fn output_path_convention_finds_nested_paths() {
        let dir = temp_dir("convention");
        let canonical = dir.canonicalize().unwrap();
        fs::write(dir.join("a.wav"), b"a").unwrap();
        fs::write(dir.join("b.npz"), b"b").unwrap();
        let output = json!({
            "clip_path": "a.wav",
            "nested": {"paths": ["b.npz"], "label": "../not-a-path"},
            "items": [{"path": "a.wav"}]
        });
        let files = validate_output_paths(&output, &canonical).unwrap();
        assert_eq!(
            files,
            vec![canonical.join("a.wav"), canonical.join("b.npz")]
        );
        let missing = validate_output_paths(&json!({"path": "nope.wav"}), &canonical);
        assert_eq!(missing.unwrap_err().code, codes::OUTPUT_MISSING);
    }

    #[test]
    fn session_serves_multiple_requests() {
        let spec = spec_or_skip!(&[]);
        let dir = temp_dir("multi");
        let mut session = WorkerSession::spawn(&spec, &["test.echo"]).unwrap();
        let cancel = AtomicBool::new(false);
        for i in 0..3 {
            let mut req = request(json!({"mode": "echo", "n": i}), &dir);
            req.id = format!("job-{i}");
            let out = session.run(&req, &cancel, &mut |_| {}).unwrap();
            assert_eq!(out.output["echo"]["n"], json!(i));
        }
        let req = WorkerRequest {
            op: "speech.tts".into(),
            ..request(json!({}), &dir)
        };
        assert_eq!(
            session.run(&req, &cancel, &mut |_| {}).unwrap_err().code,
            codes::MISSING_CAPABILITY
        );
        session.shutdown();
    }

    #[test]
    fn sha256_matches_known_vector() {
        let dir = temp_dir("sha");
        let path = dir.join("abc");
        fs::write(&path, b"abc").unwrap();
        assert_eq!(
            sha256_file(&path).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
