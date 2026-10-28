use super::*;
use crate::db::MediaDb;
use crate::jobs::JobStore;
use crate::operations::{invoke_json, OperationContext};
use crate::providers::ProviderConfig;
use crate::subtitle::SubtitleCue;
use std::net::SocketAddr;
use std::sync::Mutex;

const KEY_REF: &str = "tsubame/provider/provider-1/api-key";

/// A running endpoint over an in-memory Desktop state.
struct Desktop {
    info: ControlInfo,
    dir: PathBuf,
    segment_id: i64,
    revision: i64,
}

impl Drop for Desktop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn start_desktop() -> Desktop {
    let mut db = MediaDb::open(Path::new(":memory:")).unwrap();
    let media = db.import_media("C:/fixtures/control.wav").unwrap();
    let segment = db
        .replace_segments(
            media.id,
            &[SubtitleCue {
                start_ms: 1_000,
                end_ms: 2_000,
                text: "晚安。".into(),
            }],
            "human",
        )
        .unwrap()
        .remove(0);
    db.save_provider(&ProviderConfig {
        id: "provider-1".into(),
        name: "Example".into(),
        kind: "openai-compatible".into(),
        execution: "remote_api".into(),
        system_managed: false,
        availability: "ready".into(),
        message: String::new(),
        base_url: "".into(),
        model_list_url: "".into(),
        auth_mode: "bearer".into(),
        secret_ref: Some(KEY_REF.into()),
        enabled: true,
        last_refresh_at: None,
        last_error: None,
    })
    .unwrap();
    let state = Arc::new((Mutex::new(db), JobStore::open_in_memory().unwrap()));
    let dispatch: Dispatch = Arc::new(move |name, input| {
        let ctx = OperationContext {
            db: &state.0,
            jobs: &state.1,
        };
        invoke_json(&ctx, name, input)
    });
    let dir =
        std::env::temp_dir().join(format!("yuzuki-control-{}", uuid::Uuid::new_v4().simple()));
    let info = start(&dir, dispatch).unwrap();
    Desktop {
        info,
        dir,
        segment_id: segment.id,
        revision: segment.revision,
    }
}

fn send(addr: &str, raw: &str) -> (u16, Value) {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(raw.as_bytes()).unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).unwrap();
    let (head, body) = response.split_once("\r\n\r\n").expect("HTTP response");
    let status = head.split(' ').nth(1).unwrap().parse().unwrap();
    (status, serde_json::from_str(body).expect("JSON body"))
}

fn raw_request(
    d: &Desktop,
    method: &str,
    path: &str,
    extra_headers: &str,
    body: &str,
) -> (u16, Value) {
    let raw = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\n{extra_headers}Content-Type: application/json\r\nContent-Length: {len}\r\n\r\n{body}",
        addr = d.info.addr,
        len = body.len()
    );
    send(&d.info.addr, &raw)
}

fn authed(d: &Desktop, method: &str, path: &str, body: &str) -> (u16, Value) {
    raw_request(
        d,
        method,
        path,
        &format!("Authorization: Bearer {}\r\n", d.info.token),
        body,
    )
}

fn call(d: &Desktop, name: &str, body: Value) -> (u16, Value) {
    authed(d, "POST", &format!("/v1/ops/{name}"), &body.to_string())
}

fn error_code(body: &Value) -> &str {
    assert_eq!(body["ok"], false, "{body}");
    body["error"]["code"].as_str().unwrap()
}

#[test]
fn a_request_without_a_token_is_refused() {
    let d = start_desktop();
    for (method, path) in [
        ("GET", "/v1/ops"),
        ("GET", "/v1/health"),
        ("POST", "/v1/ops/segment.list"),
    ] {
        let (status, body) = raw_request(&d, method, path, "", "{}");
        assert_eq!(status, 401, "{method} {path}");
        assert_eq!(error_code(&body), "unauthorized");
        assert_eq!(body["error"]["message"], "missing bearer token");
    }
}

#[test]
fn a_wrong_token_is_refused() {
    let d = start_desktop();
    let wrong = "0".repeat(d.info.token.len());
    let (status, body) = raw_request(
        &d,
        "GET",
        "/v1/ops",
        &format!("Authorization: Bearer {wrong}\r\n"),
        "",
    );
    assert_eq!(status, 401);
    assert_eq!(body["error"]["message"], "invalid token");

    let prefix = &d.info.token[..d.info.token.len() - 1];
    let (status, _) = raw_request(
        &d,
        "GET",
        "/v1/ops",
        &format!("Authorization: Bearer {prefix}\r\n"),
        "",
    );
    assert_eq!(status, 401, "a token prefix is not the token");

    let (status, _) = raw_request(
        &d,
        "GET",
        "/v1/ops",
        &format!("Authorization: Basic {}\r\n", d.info.token),
        "",
    );
    assert_eq!(status, 401, "only the bearer scheme is accepted");

    // Another Desktop's token is not this one's.
    let other = start_desktop();
    assert_ne!(other.info.token, d.info.token);
    let (status, _) = raw_request(
        &d,
        "GET",
        "/v1/ops",
        &format!("Authorization: Bearer {}\r\n", other.info.token),
        "",
    );
    assert_eq!(status, 401);
}

#[test]
fn listens_on_loopback_only_and_never_on_all_interfaces() {
    let d = start_desktop();
    let addr: SocketAddr = d.info.addr.parse().unwrap();
    assert!(addr.ip().is_loopback(), "{addr}");
    assert!(!addr.ip().is_unspecified(), "{addr}");
    assert_eq!(addr.port(), d.info.port);
    assert_ne!(d.info.port, 0);
    // The token is 256 random bits, hex encoded.
    assert_eq!(d.info.token.len(), 64);
    assert!(d.info.token.chars().all(|c| c.is_ascii_hexdigit()));
    let (status, body) = authed(&d, "GET", "/v1/health", "");
    assert_eq!(status, 200);
    assert_eq!(body["result"]["protocol"], PROTOCOL_VERSION);
}

#[test]
fn control_file_tells_local_clients_where_to_connect() {
    let d = start_desktop();
    let text = std::fs::read_to_string(control_file(&d.dir)).unwrap();
    let written: ControlInfo = serde_json::from_str(&text).unwrap();
    assert_eq!(written, d.info);
    assert_eq!(written.pid, std::process::id());
    assert!(!d.dir.join(format!("{CONTROL_FILE}.tmp")).exists());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(control_file(&d.dir))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "only the current user may read the token"
        );
    }
    #[cfg(windows)]
    assert_eq!(
        dacl_sddl(&control_file(&d.dir)),
        OWNER_ONLY_SDDL,
        "only the file owner may read the token, with no inherited ACEs"
    );
}

#[test]
fn provider_list_never_exposes_a_raw_key() {
    let d = start_desktop();
    let (status, body) = call(&d, "provider.list", json!({}));
    assert_eq!(status, 200, "{body}");
    let providers = body["result"].as_array().unwrap();
    let example = providers.iter().find(|p| p["id"] == "provider-1").unwrap();
    // Only the keyring reference crosses the endpoint.
    assert_eq!(example["secret_ref"], KEY_REF);
    for provider in providers {
        for field in provider.as_object().unwrap().keys() {
            let field = field.to_ascii_lowercase();
            assert!(
                !field.contains("key")
                    && !field.contains("secret_value")
                    && !field.contains("password")
                    && !field.contains("token"),
                "provider field {field} looks like a credential"
            );
        }
    }
}

#[test]
fn operations_run_through_the_registry_with_stable_errors() {
    let d = start_desktop();
    let (status, body) = authed(&d, "GET", "/v1/ops", "");
    assert_eq!(status, 200);
    let names: Vec<&str> = body["result"]
        .as_array()
        .unwrap()
        .iter()
        .map(|op| op["name"].as_str().unwrap())
        .collect();
    let expected: Vec<&str> = registry().iter().map(|entry| entry.meta.name).collect();
    assert_eq!(names, expected);

    let update = |revision: i64| {
        json!({
            "id": d.segment_id,
            "expected_revision": revision,
            "source_text": "晚安。",
            "translated_text": "Good night.",
            "start_ms": 1_000,
            "end_ms": 2_000
        })
    };
    let (status, body) = call(&d, "segment.update", update(d.revision + 1));
    assert_eq!(status, 409);
    assert_eq!(error_code(&body), "revision_conflict");
    assert_eq!(body["error"]["message"], "segment revision conflict");

    let (status, body) = call(&d, "segment.update", update(d.revision));
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["result"]["translated_text"], "Good night.");
    assert!(body["result"]["revision"].as_i64().unwrap() > d.revision);

    let (status, body) = call(&d, "job.cancel", json!({ "id": "no-such-job" }));
    assert_eq!(status, 422);
    assert_eq!(error_code(&body), "job_store");

    let (status, body) = call(&d, "segment.delete", json!({}));
    assert_eq!(status, 404);
    assert_eq!(error_code(&body), "unknown_operation");

    let (status, body) = authed(&d, "POST", "/v1/ops/segment.list", "{not json");
    assert_eq!(status, 400);
    assert_eq!(error_code(&body), "invalid_input");

    let (status, body) = authed(&d, "GET", "/v1/ops/segment.list", "");
    assert_eq!(status, 405);
    assert_eq!(error_code(&body), "method_not_allowed");

    // An empty body is the same as {} for operations without arguments.
    let (status, _) = authed(&d, "POST", "/v1/ops/runtime.doctor", "");
    assert_eq!(status, 200);
}

#[test]
fn browser_requests_are_refused_even_with_the_token() {
    let d = start_desktop();
    // Sandboxed pages and file:// documents send the opaque origin null.
    let headers = format!("Origin: null\r\nAuthorization: Bearer {}\r\n", d.info.token);
    let (status, body) = raw_request(&d, "POST", "/v1/ops/segment.list", &headers, "{}");
    assert_eq!(status, 403);
    assert_eq!(error_code(&body), "forbidden");
}

#[test]
fn malformed_requests_get_a_json_error() {
    let d = start_desktop();
    let (status, body) = send(&d.info.addr, "NONSENSE\r\n\r\n");
    assert_eq!(status, 400);
    assert_eq!(error_code(&body), "bad_request");
}

#[test]
fn token_comparison_is_exact() {
    assert!(constant_time_eq(b"abc", b"abc"));
    assert!(!constant_time_eq(b"abc", b"abd"));
    assert!(!constant_time_eq(b"abc", b"ab"));
    assert!(!constant_time_eq(b"", b"a"));
}

/// The file's DACL as SDDL, read back from the OS.
#[cfg(windows)]
fn dacl_sddl(path: &std::path::Path) -> String {
    use std::ffi::c_void;
    use std::os::windows::ffi::OsStrExt;
    const DACL_SECURITY_INFORMATION: u32 = 0x0000_0004;
    #[link(name = "advapi32")]
    extern "system" {
        fn GetFileSecurityW(
            name: *const u16,
            info: u32,
            descriptor: *mut c_void,
            len: u32,
            needed: *mut u32,
        ) -> i32;
        fn ConvertSecurityDescriptorToStringSecurityDescriptorW(
            descriptor: *const c_void,
            revision: u32,
            info: u32,
            sddl: *mut *mut u16,
            len: *mut u32,
        ) -> i32;
    }
    #[link(name = "kernel32")]
    extern "system" {
        fn LocalFree(memory: *mut c_void) -> *mut c_void;
    }
    let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: test-only FFI; buffers are sized from the OS-reported length
    // and the returned SDDL string is NUL-terminated and freed once.
    unsafe {
        let mut needed = 0u32;
        GetFileSecurityW(
            name.as_ptr(),
            DACL_SECURITY_INFORMATION,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
        assert!(needed > 0, "{}", std::io::Error::last_os_error());
        let mut buffer = vec![0u8; needed as usize];
        let ok = GetFileSecurityW(
            name.as_ptr(),
            DACL_SECURITY_INFORMATION,
            buffer.as_mut_ptr().cast(),
            needed,
            &mut needed,
        );
        assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());
        let mut sddl: *mut u16 = std::ptr::null_mut();
        let ok = ConvertSecurityDescriptorToStringSecurityDescriptorW(
            buffer.as_ptr().cast(),
            1,
            DACL_SECURITY_INFORMATION,
            &mut sddl,
            std::ptr::null_mut(),
        );
        assert_ne!(ok, 0, "{}", std::io::Error::last_os_error());
        let len = (0..).take_while(|&i| *sddl.add(i) != 0).count();
        let text = String::from_utf16_lossy(std::slice::from_raw_parts(sddl, len));
        LocalFree(sddl.cast());
        text
    }
}
