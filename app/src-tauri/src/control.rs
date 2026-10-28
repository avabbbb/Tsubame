//! Local control endpoint (docs/07-agent-control.md, T5).
//!
//! A loopback-only HTTP/1.1 + JSON front for the Operation Registry, so the
//! yuzuki CLI and the MCP adapter drive the running Desktop without screen
//! automation. It adds no business logic: every call is invoke_json.
//!
//! - Binds 127.0.0.1 on a random port, never 0.0.0.0.
//! - Every request needs the bearer token the Desktop generated at start.
//! - The token and port live in control.json in the app data directory,
//!   readable only by the current user (mode 0600 on Unix; on Windows a
//!   protected DACL granting access to the file owner only).
//! - Requests carrying an Origin header (browsers) are refused.

use crate::operations::{registry, OperationError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

/// File in the app data directory that tells local clients where to connect.
pub const CONTROL_FILE: &str = "control.json";
/// Bumped when the wire format changes incompatibly.
pub const PROTOCOL_VERSION: u32 = 1;

const OPS_PREFIX: &str = "/v1/ops/";
const MAX_LINE: usize = 8 * 1024;
const MAX_HEADERS: usize = 64;
const MAX_BODY: usize = 4 * 1024 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(10);

/// Runs one registry operation by name. The Desktop passes invoke_json over
/// its managed state; tests pass a fixture.
pub type Dispatch = Arc<dyn Fn(&str, Value) -> Result<Value, OperationError> + Send + Sync>;

/// Contents of control.json.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlInfo {
    pub protocol: u32,
    /// Always a loopback address, as host:port.
    pub addr: String,
    pub port: u16,
    pub token: String,
    pub pid: u32,
}

pub fn control_file(dir: &Path) -> PathBuf {
    dir.join(CONTROL_FILE)
}

/// 256 bits from the OS random source, as hex.
pub fn generate_token() -> String {
    format!(
        "{}{}",
        uuid::Uuid::new_v4().simple(),
        uuid::Uuid::new_v4().simple()
    )
}

/// Starts the endpoint on its own thread and writes control.json into dir.
pub fn start(dir: &Path, dispatch: Dispatch) -> std::io::Result<ControlInfo> {
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
    let addr = listener.local_addr()?;
    let info = ControlInfo {
        protocol: PROTOCOL_VERSION,
        addr: addr.to_string(),
        port: addr.port(),
        token: generate_token(),
        pid: std::process::id(),
    };
    write_control_file(dir, &info)?;
    let token: Arc<str> = info.token.clone().into();
    std::thread::Builder::new()
        .name("yuzuki-control".into())
        .spawn(move || {
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                let token = Arc::clone(&token);
                let dispatch = Arc::clone(&dispatch);
                let _ = std::thread::Builder::new()
                    .name("yuzuki-control-conn".into())
                    .spawn(move || {
                        let _ = handle(stream, &token, dispatch.as_ref());
                    });
            }
        })?;
    Ok(info)
}

/// Written to a temporary file first, so a client never reads half a token.
fn write_control_file(dir: &Path, info: &ControlInfo) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!("{CONTROL_FILE}.tmp"));
    let _ = std::fs::remove_file(&tmp);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&tmp)?;
    // Before the token is written, so no other account can ever read it.
    #[cfg(windows)]
    restrict_to_owner(&tmp)?;
    file.write_all(&serde_json::to_vec_pretty(info)?)?;
    file.sync_all()?;
    drop(file);
    std::fs::rename(&tmp, control_file(dir))
}

struct Request {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

fn handle(
    stream: TcpStream,
    token: &str,
    dispatch: &(dyn Fn(&str, Value) -> Result<Value, OperationError> + Send + Sync),
) -> std::io::Result<()> {
    stream.set_read_timeout(Some(READ_TIMEOUT))?;
    let mut writer = stream.try_clone()?;
    let mut reader = BufReader::new(stream);
    let (status, body) = match read_request(&mut reader) {
        Ok(request) => route(&request, token, dispatch),
        Err(error) => failure(&error),
    };
    respond(&mut writer, status, &body)
}

fn bad_request(message: impl Into<String>) -> OperationError {
    OperationError::new("bad_request", false, message)
}

fn unauthorized(message: &str) -> OperationError {
    OperationError::new("unauthorized", false, message)
}

fn read_line(reader: &mut impl BufRead) -> Result<String, OperationError> {
    let mut line = String::new();
    let read = (&mut *reader)
        .take(MAX_LINE as u64)
        .read_line(&mut line)
        .map_err(|e| bad_request(e.to_string()))?;
    if read == 0 {
        return Err(bad_request("connection closed before the request ended"));
    }
    if !line.ends_with('\n') {
        return Err(bad_request("request line or header too long"));
    }
    Ok(line.trim_end_matches(['\r', '\n']).to_string())
}

fn header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

fn read_request(reader: &mut impl BufRead) -> Result<Request, OperationError> {
    let request_line = read_line(reader)?;
    let mut parts = request_line.split(' ');
    let (Some(method), Some(path), Some(version)) = (parts.next(), parts.next(), parts.next())
    else {
        return Err(bad_request("malformed request line"));
    };
    if !version.starts_with("HTTP/1.") {
        return Err(bad_request("only HTTP/1.x is supported"));
    }
    let mut headers = Vec::new();
    loop {
        let line = read_line(reader)?;
        if line.is_empty() {
            break;
        }
        if headers.len() >= MAX_HEADERS {
            return Err(bad_request("too many headers"));
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| bad_request("malformed header"))?;
        headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
    }
    if header(&headers, "transfer-encoding").is_some() {
        return Err(bad_request("chunked request bodies are not supported"));
    }
    let length = match header(&headers, "content-length") {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| bad_request("invalid content-length"))?,
        None => 0,
    };
    if length > MAX_BODY {
        return Err(OperationError::new(
            "payload_too_large",
            false,
            "request body too large",
        ));
    }
    let mut body = vec![0; length];
    reader
        .read_exact(&mut body)
        .map_err(|e| bad_request(e.to_string()))?;
    Ok(Request {
        method: method.to_string(),
        path: path.to_string(),
        headers,
        body,
    })
}

/// Compares without stopping at the first differing byte.
fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn authorize(headers: &[(String, String)], token: &str) -> Result<(), OperationError> {
    let presented = header(headers, "authorization")
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(|| unauthorized("missing bearer token"))?;
    if constant_time_eq(presented.trim().as_bytes(), token.as_bytes()) {
        Ok(())
    } else {
        Err(unauthorized("invalid token"))
    }
}

fn route(
    request: &Request,
    token: &str,
    dispatch: &(dyn Fn(&str, Value) -> Result<Value, OperationError> + Send + Sync),
) -> (u16, Value) {
    // A browser page can reach loopback; it never gets to try a token.
    if header(&request.headers, "origin").is_some() {
        return failure(&OperationError::new(
            "forbidden",
            false,
            "browser requests are not accepted",
        ));
    }
    if let Err(error) = authorize(&request.headers, token) {
        return failure(&error);
    }
    let path = request.path.split('?').next().unwrap_or_default();
    let method = request.method.as_str();
    if let Some(name) = path.strip_prefix(OPS_PREFIX) {
        if method != "POST" {
            return method_not_allowed();
        }
        let input = if request.body.iter().all(u8::is_ascii_whitespace) {
            Value::Null
        } else {
            match serde_json::from_slice(&request.body) {
                Ok(value) => value,
                Err(e) => {
                    return failure(&OperationError::new(
                        "invalid_input",
                        false,
                        format!("request body is not JSON: {e}"),
                    ))
                }
            }
        };
        return match dispatch(name, input) {
            Ok(result) => success(result),
            Err(error) => failure(&error),
        };
    }
    match (method, path) {
        ("GET", "/v1/health") => success(json!({
            "protocol": PROTOCOL_VERSION,
            "version": env!("CARGO_PKG_VERSION"),
            "pid": std::process::id(),
        })),
        ("GET", "/v1/ops") => {
            let ops: Vec<Value> = registry().iter().map(|entry| entry.describe()).collect();
            success(json!(ops))
        }
        (_, "/v1/health" | "/v1/ops") => method_not_allowed(),
        _ => failure(&OperationError::new(
            "not_found",
            false,
            format!("no route for {path}"),
        )),
    }
}

fn method_not_allowed() -> (u16, Value) {
    failure(&OperationError::new(
        "method_not_allowed",
        false,
        "method not allowed",
    ))
}

fn success(result: Value) -> (u16, Value) {
    (200, json!({ "ok": true, "result": result }))
}

fn failure(error: &OperationError) -> (u16, Value) {
    (
        status_for(&error.code),
        json!({ "ok": false, "error": error }),
    )
}

/// HTTP status for an error code. The code in the body stays the contract;
/// the status only lets generic HTTP clients branch.
fn status_for(code: &str) -> u16 {
    match code {
        "bad_request" | "invalid_input" => 400,
        "unauthorized" => 401,
        "forbidden" => 403,
        "not_found" | "unknown_operation" => 404,
        "method_not_allowed" => 405,
        "revision_conflict" => 409,
        "payload_too_large" => 413,
        "internal" | "state_poisoned" | "db" => 500,
        // The operation ran and failed (cancelled, provider error, ...).
        _ => 422,
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        409 => "Conflict",
        413 => "Payload Too Large",
        422 => "Unprocessable Entity",
        _ => "Internal Server Error",
    }
}

fn respond(stream: &mut TcpStream, status: u16, body: &Value) -> std::io::Result<()> {
    let body = serde_json::to_vec(body)?;
    write!(
        stream,
        "HTTP/1.1 {status} {}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        reason(status),
        body.len()
    )?;
    stream.write_all(&body)?;
    stream.flush()
}

#[cfg(test)]
mod tests;

/// Protected DACL with one ACE: full access for OWNER RIGHTS. Inherited
/// AppData ACEs (SYSTEM, Administrators) stop applying to the token.
#[cfg(windows)]
const OWNER_ONLY_SDDL: &str = "D:P(A;;FA;;;OW)";

#[cfg(windows)]
fn restrict_to_owner(path: &Path) -> std::io::Result<()> {
    use std::ffi::{c_void, OsStr};
    use std::os::windows::ffi::OsStrExt;
    const SDDL_REVISION_1: u32 = 1;
    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    const PROTECTED_DACL_SECURITY_INFORMATION: u32 = 0x8000_0000;
    #[link(name = "advapi32")]
    extern "system" {
        fn ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl: *const u16,
            revision: u32,
            descriptor: *mut *mut c_void,
            size: *mut u32,
        ) -> i32;
        fn SetFileSecurityW(name: *const u16, info: u32, descriptor: *mut c_void) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }
    let wide = |s: &OsStr| s.encode_wide().chain(Some(0)).collect::<Vec<u16>>();
    let sddl = wide(OsStr::new(OWNER_ONLY_SDDL));
    let name = wide(path.as_os_str());
    let mut descriptor = std::ptr::null_mut();
    // SAFETY: both strings are NUL-terminated UTF-16 that outlive the calls;
    // the descriptor is allocated by the OS and freed exactly once.
    unsafe {
        if ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            std::ptr::null_mut(),
        ) == 0
        {
            return Err(std::io::Error::last_os_error());
        }
        let applied = SetFileSecurityW(
            name.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        );
        let error = std::io::Error::last_os_error();
        LocalFree(descriptor);
        if applied == 0 {
            Err(error)
        } else {
            Ok(())
        }
    }
}
