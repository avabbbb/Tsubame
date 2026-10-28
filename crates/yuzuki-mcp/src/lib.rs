//! MCP server (stdio, newline-delimited JSON-RPC 2.0) for the YUZUKI Desktop
//! (docs/07-agent-control.md).
//!
//! Every tool is one operation of the Desktop's Operation Registry: tools/list
//! reads GET /v1/ops live and tools/call goes through POST /v1/ops/<name> via
//! yuzuki-client. There is no MCP-only business path; this crate only
//! translates envelopes. Tool names replace the dots of operation names with
//! underscores (segment.update -> segment_update), since many MCP clients
//! accept only [A-Za-z0-9_-] in tool names.

use serde_json::{json, Value};
use yuzuki_client::{ClientError, ControlClient, DiscoverOptions, OpMeta};

/// MCP revisions this server speaks, newest first.
pub const PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

pub const PARSE_ERROR: i64 = -32700;
pub const INVALID_REQUEST: i64 = -32600;
pub const METHOD_NOT_FOUND: i64 = -32601;
pub const INVALID_PARAMS: i64 = -32602;
/// Server-defined: the Desktop could not be reached or answered badly.
/// error.data.code carries the yuzuki-client error code
/// (desktop_not_running, launch_failed, transport, ...).
pub const DESKTOP_ERROR: i64 = -32000;

/// Stateless apart from discovery options: every request re-discovers the
/// Desktop, so a restarted Desktop (new port and token) is picked up and a
/// missing one is reported, never papered over.
pub struct Server {
    opts: DiscoverOptions,
}

struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl RpcError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            data: None,
        }
    }

    fn desktop(error: ClientError) -> Self {
        Self {
            code: DESKTOP_ERROR,
            message: error.to_string(),
            data: Some(json!({ "code": error.code, "retryable": error.retryable })),
        }
    }
}

/// MCP tool name of an operation.
pub fn tool_name(operation: &str) -> String {
    operation.replace('.', "_")
}

impl Server {
    pub fn new(opts: DiscoverOptions) -> Self {
        Self { opts }
    }

    /// Handles one line of the stdio transport. None for notifications,
    /// client responses and blank lines.
    pub fn handle_line(&self, line: &str) -> Option<String> {
        let line = line.trim();
        if line.is_empty() {
            return None;
        }
        match serde_json::from_str::<Value>(line) {
            Ok(message) => self.handle(message).map(|reply| reply.to_string()),
            Err(e) => Some(
                error_response(
                    Value::Null,
                    PARSE_ERROR,
                    &format!("invalid JSON: {e}"),
                    None,
                )
                .to_string(),
            ),
        }
    }

    /// Handles one JSON-RPC message.
    pub fn handle(&self, message: Value) -> Option<Value> {
        let Some(object) = message.as_object() else {
            // Batches were removed in MCP 2025-06-18.
            return Some(error_response(
                Value::Null,
                INVALID_REQUEST,
                "expected a single JSON-RPC object",
                None,
            ));
        };
        let id = object.get("id").cloned();
        let Some(method) = object.get("method").and_then(Value::as_str) else {
            // This server sends no requests, so this is not a response we await.
            return id.map(|id| error_response(id, INVALID_REQUEST, "missing method", None));
        };
        // Notifications (initialized, cancelled) need no reply.
        let id = id?;
        let params = object.get("params").cloned().unwrap_or(Value::Null);
        let result = match method {
            "initialize" => Ok(initialize(&params)),
            "ping" => Ok(json!({})),
            "tools/list" => self.list_tools(),
            "tools/call" => self.call_tool(&params),
            _ => Err(RpcError::new(
                METHOD_NOT_FOUND,
                format!("unknown method {method}"),
            )),
        };
        Some(match result {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err(e) => error_response(id, e.code, &e.message, e.data),
        })
    }

    fn connect(&self) -> Result<ControlClient, RpcError> {
        ControlClient::discover(&self.opts).map_err(RpcError::desktop)
    }

    fn list_tools(&self) -> Result<Value, RpcError> {
        let ops = self.connect()?.list_ops().map_err(RpcError::desktop)?;
        let tools: Vec<Value> = ops.iter().map(tool).collect();
        Ok(json!({ "tools": tools }))
    }

    fn call_tool(&self, params: &Value) -> Result<Value, RpcError> {
        let name = params
            .get("name")
            .and_then(Value::as_str)
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, "tools/call needs a string name"))?;
        let arguments = match params.get("arguments") {
            None | Some(Value::Null) => json!({}),
            Some(arguments @ Value::Object(_)) => arguments.clone(),
            Some(_) => {
                return Err(RpcError::new(
                    INVALID_PARAMS,
                    "tools/call arguments must be an object",
                ))
            }
        };
        let client = self.connect()?;
        let ops = client.list_ops().map_err(RpcError::desktop)?;
        let mut matches = ops.iter().filter(|op| tool_name(&op.name) == name);
        let op = matches
            .next()
            .ok_or_else(|| RpcError::new(INVALID_PARAMS, format!("unknown tool {name}")))?;
        if let Some(other) = matches.next() {
            return Err(RpcError::new(
                INVALID_PARAMS,
                format!("tool {name} is ambiguous: {} and {}", op.name, other.name),
            ));
        }
        // Operation failures (revision_conflict, not_found, ...) are tool
        // results with isError, so the model sees the server's error code.
        Ok(match client.call(&op.name, arguments) {
            Ok(result) => tool_result(json!({ "result": result }), false),
            Err(error) => tool_result(json!({ "error": error }), true),
        })
    }
}

fn initialize(params: &Value) -> Value {
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_str)
        .filter(|requested| PROTOCOL_VERSIONS.contains(requested))
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": { "tools": { "listChanged": false } },
        "serverInfo": { "name": "yuzuki-mcp", "version": env!("CARGO_PKG_VERSION") },
        "instructions": "Tools are the YUZUKI Desktop's Operation Registry. Mutating tools that take expected_revision fail with revision_conflict when it is stale; re-read and retry.",
    })
}

fn tool(op: &OpMeta) -> Value {
    json!({
        "name": tool_name(&op.name),
        "title": op.name,
        "description": description(op),
        "inputSchema": input_schema(&op.input_schema),
        "annotations": { "readOnlyHint": !op.mutates },
    })
}

fn description(op: &OpMeta) -> String {
    let mut text = format!(
        "YUZUKI operation {} ({}).",
        op.name,
        if op.mutates { "mutates" } else { "read-only" }
    );
    if op.expected_revision {
        text.push_str(" Requires expected_revision; a stale value fails with revision_conflict and changes nothing.");
    }
    if !op.side_effects.is_empty() {
        let effects: Vec<String> = op
            .side_effects
            .iter()
            .map(|effect| match effect.as_str() {
                Some(name) => name.to_string(),
                None => effect.to_string(),
            })
            .collect();
        text.push_str(&format!(" Side effects: {}.", effects.join(", ")));
    }
    text
}

/// MCP requires an object schema; an operation without arguments may
/// publish another type (it accepts null and {}).
fn input_schema(schema: &Value) -> Value {
    match schema.get("type") {
        Some(kind) if kind == "object" => schema.clone(),
        _ => json!({ "type": "object", "properties": {} }),
    }
}

fn tool_result(structured: Value, is_error: bool) -> Value {
    let text = serde_json::to_string_pretty(&structured).unwrap_or_else(|_| structured.to_string());
    json!({
        "content": [{ "type": "text", "text": text }],
        "structuredContent": structured,
        "isError": is_error,
    })
}

fn error_response(id: Value, code: i64, message: &str, data: Option<Value>) -> Value {
    let mut error = json!({ "code": code, "message": message });
    if let Some(data) = data {
        error["data"] = data;
    }
    json!({ "jsonrpc": "2.0", "id": id, "error": error })
}
