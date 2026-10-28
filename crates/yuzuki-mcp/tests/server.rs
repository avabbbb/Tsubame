//! yuzuki-mcp against a real control endpoint over an in-memory Desktop.
//! Nothing here spawns processes (see crates/yuzuki-cli/tests/golden.rs).

use serde_json::{json, Value};
use tsubame_lib::test_support::{start_fixture_desktop, FixtureDesktop};
use yuzuki_client::DiscoverOptions;
use yuzuki_mcp::{Server, DESKTOP_ERROR, INVALID_PARAMS, METHOD_NOT_FOUND, PARSE_ERROR};

fn server_for(d: &FixtureDesktop) -> Server {
    Server::new(DiscoverOptions {
        control_file: Some(d.control_file.clone()),
        ..DiscoverOptions::default()
    })
}

fn request(server: &Server, method: &str, params: Value) -> Value {
    server
        .handle(json!({ "jsonrpc": "2.0", "id": 1, "method": method, "params": params }))
        .expect("a request gets a reply")
}

fn result(reply: &Value) -> &Value {
    assert!(reply.get("error").is_none(), "{reply}");
    &reply["result"]
}

fn error_code(reply: &Value) -> i64 {
    reply["error"]["code"]
        .as_i64()
        .unwrap_or_else(|| panic!("no error: {reply}"))
}

fn call(server: &Server, name: &str, arguments: Value) -> Value {
    let reply = request(
        server,
        "tools/call",
        json!({ "name": name, "arguments": arguments }),
    );
    result(&reply).clone()
}

#[test]
fn initialize_negotiates_a_supported_protocol() {
    let d = start_fixture_desktop();
    let s = server_for(&d);
    let known = request(&s, "initialize", json!({ "protocolVersion": "2025-03-26" }));
    assert_eq!(result(&known)["protocolVersion"], "2025-03-26");
    assert_eq!(result(&known)["serverInfo"]["name"], "yuzuki-mcp");
    let unknown = request(&s, "initialize", json!({ "protocolVersion": "1999-01-01" }));
    assert_eq!(
        result(&unknown)["protocolVersion"],
        yuzuki_mcp::PROTOCOL_VERSIONS[0]
    );
}

#[test]
fn notifications_get_no_reply() {
    let d = start_fixture_desktop();
    let s = server_for(&d);
    let reply = s.handle(json!({ "jsonrpc": "2.0", "method": "notifications/initialized" }));
    assert!(reply.is_none(), "{reply:?}");
    assert!(s.handle_line("   ").is_none());
}

#[test]
fn bad_json_and_unknown_methods_are_rpc_errors() {
    let d = start_fixture_desktop();
    let s = server_for(&d);
    let parsed: Value = serde_json::from_str(&s.handle_line("{not json").unwrap()).unwrap();
    assert_eq!(error_code(&parsed), PARSE_ERROR);
    assert_eq!(
        error_code(&request(&s, "resources/list", json!({}))),
        METHOD_NOT_FOUND
    );
}

#[test]
fn tools_list_mirrors_the_operation_registry() {
    let d = start_fixture_desktop();
    let s = server_for(&d);
    let reply = request(&s, "tools/list", json!({}));
    let tools = result(&reply)["tools"].as_array().unwrap().clone();
    let find = |name: &str| {
        tools
            .iter()
            .find(|tool| tool["name"] == name)
            .unwrap_or_else(|| panic!("{name} missing"))
            .clone()
    };
    for name in [
        "segment_list",
        "segment_update",
        "segment_regenerate",
        "job_list",
        "job_cancel",
        "runtime_doctor",
        "provider_list",
        "runtime_plan",
        "runtime_install",
        "media_list",
        "media_import_folder",
        "asr_run",
    ] {
        let tool = find(name);
        assert_eq!(tool["inputSchema"]["type"], "object", "{tool}");
        assert!(tool["name"]
            .as_str()
            .unwrap()
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'));
    }
    assert_eq!(find("segment_list")["title"], "segment.list");
    assert_eq!(find("segment_list")["annotations"]["readOnlyHint"], true);
    assert_eq!(find("segment_update")["annotations"]["readOnlyHint"], false);
    assert!(find("segment_update")["description"]
        .as_str()
        .unwrap()
        .contains("expected_revision"));
}

#[test]
fn segment_list_returns_the_fixture_segment() {
    let d = start_fixture_desktop();
    let s = server_for(&d);
    let out = call(&s, "segment_list", json!({ "media_id": d.media_id }));
    assert_eq!(out["isError"], false, "{out}");
    let segments = out["structuredContent"]["result"].as_array().unwrap();
    assert_eq!(segments.len(), 1, "{out}");
    assert_eq!(segments[0]["id"], d.segment_id);
    assert_eq!(out["content"][0]["type"], "text");
}

#[test]
fn a_stale_revision_is_a_tool_error_and_changes_nothing() {
    let d = start_fixture_desktop();
    let s = server_for(&d);
    let args = json!({
        "id": d.segment_id,
        "expected_revision": d.revision,
        "translated_text": "Good night.",
    });
    let first = call(&s, "segment_update", args.clone());
    assert_eq!(first["isError"], false, "{first}");
    let listed = call(&s, "segment_list", json!({ "media_id": d.media_id }));
    let after_first = listed["structuredContent"]["result"].clone();

    let stale = call(
        &s,
        "segment_update",
        json!({
            "id": d.segment_id,
            "expected_revision": d.revision,
            "translated_text": "Overwritten.",
        }),
    );
    assert_eq!(stale["isError"], true, "{stale}");
    assert_eq!(
        stale["structuredContent"]["error"]["code"],
        "revision_conflict"
    );
    let relisted = call(&s, "segment_list", json!({ "media_id": d.media_id }));
    assert_eq!(relisted["structuredContent"]["result"], after_first);
}

#[test]
fn unknown_input_fields_are_invalid_input_tool_errors() {
    let d = start_fixture_desktop();
    let s = server_for(&d);
    let out = call(&s, "segment_list", json!({ "bogus": 1 }));
    assert_eq!(out["isError"], true, "{out}");
    assert_eq!(out["structuredContent"]["error"]["code"], "invalid_input");
}

#[test]
fn bad_tool_calls_are_invalid_params() {
    let d = start_fixture_desktop();
    let s = server_for(&d);
    let unknown = request(&s, "tools/call", json!({ "name": "segment.list" }));
    assert_eq!(error_code(&unknown), INVALID_PARAMS);
    let no_name = request(&s, "tools/call", json!({}));
    assert_eq!(error_code(&no_name), INVALID_PARAMS);
    let bad_args = request(
        &s,
        "tools/call",
        json!({ "name": "job_list", "arguments": [1] }),
    );
    assert_eq!(error_code(&bad_args), INVALID_PARAMS);
}

#[test]
fn a_missing_desktop_is_a_desktop_error() {
    let missing = std::env::temp_dir()
        .join("yuzuki-mcp-no-such-dir")
        .join("control.json");
    let s = Server::new(DiscoverOptions {
        control_file: Some(missing),
        ..DiscoverOptions::default()
    });
    let reply = request(&s, "tools/list", json!({}));
    assert_eq!(error_code(&reply), DESKTOP_ERROR);
    assert_eq!(reply["error"]["data"]["code"], "desktop_not_running");
    // initialize and ping never need the Desktop.
    assert!(request(&s, "ping", json!({})).get("result").is_some());
}
