# Agent Control and YUZUKI Skill

## Goal

A local coding/general agent should be able to operate the installed YUZUKI Desktop without clicking the UI.

The Desktop remains the canonical state owner.

## Control stack

```text
Local Agent
    │
  $yuzuki Skill
    │
 ┌──┴───────────┐
 │              │
CLI            MCP
 │              │
 └────┬─────────┘
      │
  Local RPC
      │
YUZUKI Desktop
      │
Operation Registry
```

## Operation Registry contract

Implemented in `app/src-tauri/src/operations.rs` (tests in `operations/tests.rs`). Every surface (Tauri commands, the local control endpoint, CLI, MCP) calls the same registry; none re-implements an operation.

Each operation implements the `Operation` trait:
- `Input`: a serde `Deserialize` type. JSON dispatch deserializes into it, so malformed input fails as `invalid_input` before the operation runs.
- `Output`: a serde `Serialize` type; the JSON result is exactly its serialization.
- `META: OperationMeta`: a stable dotted `name`, whether it `mutates`, its declared `side_effects`, whether it takes an `expected_revision`, and its `cancellation` kind (`None`, or `Job` when the work runs as a cancellable Job).
- `run(&OperationContext, Input)`: the only implementation.

Two entry points share that `run`:
- `invoke::<O>(ctx, input)`: typed, used by the Tauri command wrappers, which keep their existing argument names and response shapes.
- `invoke_json(ctx, name, value)`: by name, for the control endpoint, CLI and MCP. An unknown name fails as `unknown_operation`.

Errors are `OperationError { code, message, retryable }`. Messages keep the text the Desktop showed before the registry (for example `segment revision conflict` and `not_found: segment not found`), so the UI needs no change.

Registered operations (first slice):

| Name | Mutates | Expected revision | Cancellation |
|---|---|---|---|
| `segment.list` | no | no | none |
| `segment.update` | yes | yes | none |
| `segment.regenerate` | yes | yes | Job |
| `job.list` | no | no | none |
| `job.cancel` | yes | no | none |
| `runtime.doctor` | no | no | none |
| `provider.list` | no | no | none |

Invariants, covered by tests:
- names are unique and the metadata matches this table;
- typed and JSON dispatch return the same result or the same error for every operation;
- a stale `expected_revision` is refused with `revision_conflict` and changes nothing;
- cancelling an unknown Job is an error, not a no-op.

Not yet: machine-readable JSON Schema for each `Input`/`Output` (planned with the CLI/MCP schema output in T7, likely via `schemars`), and registering the remaining Desktop commands.

## Local control endpoint

Implemented in `app/src-tauri/src/control.rs` (tests in `control/tests.rs`). The Desktop starts it during setup; if it cannot bind, the Desktop logs that and keeps working.

- Binds `127.0.0.1` on a random port, never `0.0.0.0`.
- At start the Desktop generates a 256-bit token and writes `control.json` (`protocol`, `addr`, `port`, `token`, `pid`) to its app-data directory through a temporary file and a rename. On Unix the file is mode `0600`; on Windows it inherits the per-user AppData ACL.
- Every request sends `Authorization: Bearer <token>`. A missing or wrong token gets `401 unauthorized`. A request with an `Origin` header (a browser page) gets `403 forbidden`, token or not.
- Routes: `GET /v1/health`; `GET /v1/ops` (registry metadata); `POST /v1/ops/<name>` with the operation's JSON input as the body (an empty body means no input), dispatched through `invoke_json`.
- Responses are JSON: `{"ok": true, "result": ...}` or `{"ok": false, "error": {"code", "message", "retryable"}}`. The `code` is the contract; the HTTP status only mirrors it: 400 `bad_request` or `invalid_input`, 401, 403, 404 `not_found` or `unknown_operation`, 405, 409 `revision_conflict`, 413, 500 for internal and database failures, 422 for any other operation failure.
- `provider.list` returns provider configs carrying only `secret_ref`, an OS keyring reference; raw provider keys never cross the endpoint.
- HTTP/1.1, one request per connection, no chunked bodies, a 4 MiB body cap and a 10 s read timeout.

## Why a Skill

The Skill provides host-agnostic instructions for:
- locating the install;
- bootstrapping when necessary;
- opening/connecting to YUZUKI;
- running diagnostics;
- resolving/installing required models/runtimes;
- invoking stable CLI/MCP operations;
- understanding mutation/revision semantics.

It must not contain provider-specific business logic duplicated from the app.

## CLI

Implemented in crates/yuzuki-cli (binary yuzuki) on top of crates/yuzuki-client. Both are clients of the local control endpoint only: each command maps its flags onto one Operation Registry input and prints the result. Validation, revisions and every other rule stay in the Desktop.

Discovery: --control-file, else YUZUKI_CONTROL_FILE, else <app data dir>/com.avabbbb.tsubame/control.json (%APPDATA% on Windows, ~/Library/Application Support on macOS, $XDG_DATA_HOME or ~/.local/share on Linux). The client checks GET /v1/health; a missing file, or one nobody answers, is desktop_not_running. With --launch it starts the Desktop (YUZUKI_DESKTOP, else YUZUKI/yuzuki/tsubame next to the yuzuki binary) and polls for up to 30 s; without --launch it never starts anything. app status never launches. The launch path has not yet been exercised against an installed Desktop.

~~~bash
yuzuki doctor --json                                   # runtime.doctor
yuzuki app status --json                               # health, pid, addr
yuzuki ops list --json                                 # GET /v1/ops with input schemas
yuzuki segment list --media-id <id> --json
yuzuki segment update --id <id> --expected-revision <n> [--source-text ..] [--translated-text ..] [--start-ms n] [--end-ms n] --json
yuzuki segment regenerate --segment-id <id> --expected-revision <n> [--voice-profile-id ..] [--spatial-mode ..] --json
yuzuki job list [--media-id n] [--segment-id n] [--kind ..] [--status ..] [--limit n] --json
yuzuki job cancel --id <job-id> --json
yuzuki runtime plan --provider-id <id> [--model-id <id>] --json
yuzuki runtime install --provider-id <id> [--model-id <id>] --json
yuzuki call <operation> --input '<json>' --json         # any registered operation
~~~

--json prints exactly one envelope on stdout: {"ok":true,"data":...} or {"ok":false,"error":{"code":...,"message":...}}. Without it, data goes to stdout and error[code]: message to stderr.

| Exit | Error codes |
|---|---|
| 0 | success |
| 1 | anything else (transport, bad_response, protocol_mismatch, provider errors, ...) |
| 2 | usage |
| 3 | desktop_not_running, launch_failed |
| 4 | unauthorized |
| 5 | revision_conflict |
| 6 | not_found |
| 7 | invalid_input |
| 8 | cancelled, superseded |

Tests: crates/yuzuki-client/src/tests.rs (discovery, stale file, protocol, envelopes, token redaction), crates/yuzuki-cli/tests/golden.rs (golden JSON in tests/golden/ against the real endpoint over an in-memory Desktop from the tsubame test-support feature, including a stale revision; regenerate with YUZUKI_BLESS=1), and crates/yuzuki-cli/tests/exit_codes.rs (process exit codes).

Known issue: on Windows a child process spawned while the endpoint accepts a connection can inherit the accepted socket, and the client then sees os error 10054. The tests keep process spawning in a separate test binary. The Desktop also spawns workers in-process, so the same rare reset can occur there; this is not fixed yet.

Not yet bundled: the yuzuki binary is not configured as a Tauri sidecar (bundle.externalBin), so installers do not ship it yet.

Planned, not implemented: app ensure, init, provider list/models on the CLI (the provider.list operation is reachable through yuzuki call provider.list), profile list/resolve, profile-keyed runtime plan/install, the source commands, library list, work/track show, segment show, the process commands, job show/retry.

## Agent end-to-end test

crates/yuzuki-cli/tests/agent_e2e.rs drives the docs/01-product.md flow through yuzuki --json only (yuzuki_cli::run in-process; no GUI, no direct operation calls, no process spawning) against the real control endpoint over a file-backed fixture Desktop (test_support::start_file_backed_desktop, feature test-support). At the end it opens the fixture's SQLite file read-only and checks segment text, revision, dirty flags, the active take and the job rows. CI runs it on every desktop-check matrix OS as a required step: cargo test -p yuzuki-cli --all-features --locked --test agent_e2e.

The fixture registers a test-only regenerate runner that synthesizes with MockTts directly. It never goes through tts::adapter_for_provider, so no Provider kind selects the mock, and none of it is compiled without test-support.

| Step | Operation | Reachable | Evidence |
|---|---|---|---|
| 1 install | none (app status only checks the endpoint) | no | yuzuki-cli lib.rs: AppCommand::Status |
| 2 import local media | media.import_folder, media.list | yes | operations.rs: MediaImportFolder, MediaList |
| 3 choose/install ASR provider | provider.list (via call), runtime.plan, runtime.install | partial: list is run; install is not (downloads packs) | operations.rs: ProviderList, RuntimeInstall |
| 4 generate sentence subtitles | asr.run (+ job.list) | yes (queues the asr job; needs an installed ASR runtime) | operations.rs: AsrRun |
| 5 edit one sentence | segment.update | yes | operations.rs: SegmentUpdate |
| 6 BYOK translation provider | none | no | operations.rs registry: no provider or secret write op |
| 7 translate | none | no | operations.rs registry: no translate op |
| 8 configure TTS runtime/voice | none (fixture seeds the voice profile) | no | voice.rs: save is not registered |
| 9 regenerate only that sentence | segment.regenerate | yes, with the test-support mock runner | operations.rs: SegmentRegenerate; test_support.rs: mock_regenerate_runner |
| 10 continue playback with the generated variant | none (the active take is checked in SQLite) | no | operations.rs registry: no playback op |

Each unreachable step is a named #[ignore = "gap: ..."] test in agent_e2e.rs. A full 10-step run needs registry ops for import, ASR, provider/secret configuration, translate, TTS/voice profile configuration and playback, plus a runtime.install fixture that does not download.

## MCP

Implemented in crates/yuzuki-mcp (binary yuzuki-mcp): an MCP server over stdio, newline-delimited JSON-RPC 2.0, protocol revisions 2025-06-18, 2025-03-26 and 2024-11-05. Like the CLI it is a client of the local control endpoint through crates/yuzuki-client and holds no business logic; there is no MCP-only path.

~~~bash
yuzuki-mcp [--control-file <path>] [--launch]
~~~

- Discovery is the CLI's: --control-file, else YUZUKI_CONTROL_FILE, else the per-user control.json. Every request re-discovers the Desktop, so a restarted Desktop (new port and token) is picked up. --launch starts the Desktop when nothing answers.
- tools/list reads GET /v1/ops live: one tool per registered operation. The tool name is the operation name with dots replaced by underscores (segment.update -> segment_update), title is the dotted name, inputSchema is the operation's JSON Schema (an object schema; operations without arguments publish an empty object), annotations.readOnlyHint is !mutates, and the description states expected_revision and side effects.
- tools/call runs POST /v1/ops/<name>. A result is structuredContent {"result": ...} with isError false. An operation failure (revision_conflict, invalid_input, not_found, ...) is a tool result with isError true and structuredContent {"error": {code, message, retryable}}, so the model sees the stable code.
- JSON-RPC errors: -32700 invalid JSON, -32600 not a single request object, -32601 unknown method, -32602 unknown tool or bad arguments, -32000 the Desktop could not be reached (error.data.code is the yuzuki-client code: desktop_not_running, launch_failed, transport, protocol_mismatch, bad_response). initialize and ping never need the Desktop.
- stdout carries only JSON-RPC; diagnostics go to stderr.

Tests: crates/yuzuki-mcp/tests/server.rs against the real control endpoint over the in-memory fixture Desktop (feature test-support). Not yet bundled as a Tauri sidecar, like the yuzuki binary.

## Mutation policy

Read-only calls execute directly.

Mutating calls use:
- typed arguments;
- expected revision where relevant;
- explicit operation result;
- audit entry.

Potentially costly actions should expose a plan:
- large runtime/model downloads;
- paid remote inference;
- bulk processing/acquisition.

## Desktop interaction

Agents should not need screen automation.

A running app exposes the authenticated localhost-only control endpoint described above. The CLI starts/connects to the Desktop and calls the Operation Registry through it.

## Unattended workflows

Supported long-term examples:
- import a folder and generate subtitles for new Japanese tracks;
- use a chosen profile on all selected tracks;
- regenerate only dirty sentences;
- monitor download/processing jobs;
- export completed subtitle/dub variants.

Unattended mode must still respect cost, rights and mutation policies.
