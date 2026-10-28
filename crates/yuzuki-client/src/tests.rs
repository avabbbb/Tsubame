use super::*;
use serde_json::json;

fn temp_dir() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("yuzuki-client-{}-{nanos}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn opts(path: PathBuf) -> DiscoverOptions {
    DiscoverOptions {
        control_file: Some(path),
        ..DiscoverOptions::default()
    }
}

#[test]
fn a_missing_control_file_means_the_desktop_is_not_running() {
    let dir = temp_dir();
    let error = ControlClient::discover(&opts(dir.join(CONTROL_FILE))).unwrap_err();
    assert_eq!(error.code, "desktop_not_running", "{error}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn a_control_file_nobody_answers_is_stale() {
    let dir = temp_dir();
    // A port that was free a moment ago: nothing listens on it now.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let info = json!({"protocol": PROTOCOL_VERSION, "addr": format!("127.0.0.1:{port}"), "port": port, "token": "t", "pid": 1});
    let path = dir.join(CONTROL_FILE);
    std::fs::write(&path, info.to_string()).unwrap();
    let error = ControlClient::discover(&opts(path)).unwrap_err();
    assert_eq!(error.code, "desktop_not_running", "{error}");
    assert!(error.message.contains("stale"), "{error}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn another_protocol_is_refused_before_connecting() {
    let dir = temp_dir();
    let info = json!({"protocol": PROTOCOL_VERSION + 1, "addr": "127.0.0.1:1", "port": 1, "token": "t", "pid": 1});
    let path = dir.join(CONTROL_FILE);
    std::fs::write(&path, info.to_string()).unwrap();
    let error = ControlClient::discover(&opts(path)).unwrap_err();
    assert_eq!(error.code, "protocol_mismatch", "{error}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn server_errors_keep_their_code() {
    let raw = b"HTTP/1.1 409 Conflict\r\nContent-Type: application/json\r\n\r\n{\"ok\":false,\"error\":{\"code\":\"revision_conflict\",\"message\":\"stale\",\"retryable\":false}}";
    let error = envelope(parse_response(raw).unwrap()).unwrap_err();
    assert_eq!(error, ClientError::new("revision_conflict", "stale"));
}

#[test]
fn success_unwraps_the_result() {
    let raw = b"HTTP/1.1 200 OK\r\n\r\n{\"ok\":true,\"result\":[1,2]}";
    assert_eq!(
        envelope(parse_response(raw).unwrap()).unwrap(),
        json!([1, 2])
    );
}

#[test]
fn responses_without_an_envelope_are_bad_responses() {
    for raw in [
        &b"HTTP/1.1 200 OK\r\n\r\nnot json"[..],
        b"garbage",
        b"HTTP/1.1 200 OK\r\n\r\n{}",
    ] {
        let error = parse_response(raw).and_then(envelope).unwrap_err();
        assert_eq!(error.code, "bad_response", "{error}");
    }
}

#[test]
fn debug_output_never_contains_the_token() {
    let client = ControlClient {
        info: ControlInfo {
            protocol: PROTOCOL_VERSION,
            addr: "127.0.0.1:1".into(),
            port: 1,
            token: "secret-token-value".into(),
            pid: 1,
        },
        control_file: PathBuf::from(CONTROL_FILE),
    };
    assert!(!format!("{client:?}").contains("secret-token-value"));
}
