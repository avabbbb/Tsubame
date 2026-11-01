//! yuzuki: command-line client for the YUZUKI Desktop control endpoint.
//!
//! Each command maps its flags onto one Operation Registry input and calls
//! it through yuzuki-client. Validation, revisions and every other rule
//! stay in the Desktop; this crate only parses arguments and prints.
//!
//! Exit codes (stable): 0 success, 1 other, 2 usage,
//! 3 desktop_not_running/launch_failed, 4 unauthorized,
//! 5 revision_conflict, 6 not_found, 7 invalid_input,
//! 8 cancelled/superseded.

use clap::{Args, Parser, Subcommand};
use serde_json::{json, Map, Value};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use yuzuki_client::{ClientError, ControlClient, DiscoverOptions};

#[derive(Parser)]
#[command(
    name = "yuzuki",
    version,
    about = "Control a running YUZUKI Desktop through its local endpoint"
)]
struct Cli {
    /// Print one JSON envelope: {ok:true,data} or {ok:false,error:{code,message}}.
    #[arg(long, global = true)]
    json: bool,
    /// control.json to use instead of YUZUKI_CONTROL_FILE / the app data directory.
    #[arg(long, global = true, value_name = "PATH")]
    control_file: Option<PathBuf>,
    /// Start the Desktop when it is not running (YUZUKI_DESKTOP or next to this binary).
    #[arg(long, global = true)]
    launch: bool,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Runtime health report (runtime.doctor).
    Doctor,
    /// Bootstrap report for agents: CLI and MCP paths (with an MCP client
    /// config), Desktop state and runtime.doctor. Starts the Desktop only
    /// with --launch; a Desktop that is not running is reported, not an error.
    Init,
    /// The Desktop process itself.
    #[command(subcommand)]
    App(AppCommand),
    /// The Operation Registry.
    #[command(subcommand)]
    Ops(OpsCommand),
    #[command(subcommand)]
    Segment(SegmentCommand),
    #[command(subcommand)]
    Job(JobCommand),
    #[command(subcommand)]
    Runtime(RuntimeCommand),
    #[command(subcommand)]
    Media(MediaCommand),
    #[command(subcommand)]
    Asr(AsrCommand),
    #[command(subcommand)]
    Translate(TranslateCommand),
    #[command(subcommand)]
    Tts(TtsCommand),
    #[command(subcommand)]
    Spatial(SpatialCommand),
    #[command(subcommand)]
    Mix(MixCommand),
    #[command(subcommand)]
    Source(SourceCommand),
    #[command(subcommand)]
    Download(DownloadCommand),
    /// DLsite account (sign-in itself happens in the Desktop).
    #[command(subcommand)]
    Dlsite(DlsiteCommand),
    /// Call any registered operation with a raw JSON input.
    Call {
        name: String,
        #[arg(long, default_value = "{}")]
        input: String,
    },
}

#[derive(Subcommand)]
enum AppCommand {
    /// Whether the Desktop endpoint answers; never launches it.
    Status,
    /// Start the Desktop only when it is not running, then report it.
    Ensure,
}

#[derive(Subcommand)]
enum OpsCommand {
    /// Registered operations with their metadata and input schemas.
    List,
}

#[derive(Subcommand)]
enum SegmentCommand {
    /// segment.list
    List {
        #[arg(long)]
        media_id: i64,
    },
    /// segment.update; omitted fields keep their stored value.
    Update {
        #[arg(long)]
        id: i64,
        #[arg(long)]
        expected_revision: i64,
        #[arg(long)]
        source_text: Option<String>,
        #[arg(long)]
        translated_text: Option<String>,
        #[arg(long)]
        start_ms: Option<i64>,
        #[arg(long)]
        end_ms: Option<i64>,
    },
    /// segment.regenerate; waits for the job to finish.
    Regenerate {
        #[arg(long)]
        segment_id: i64,
        #[arg(long)]
        expected_revision: i64,
        #[arg(long)]
        voice_profile_id: Option<String>,
        #[arg(long)]
        spatial_mode: Option<String>,
    },
}

#[derive(Subcommand)]
enum JobCommand {
    /// job.list
    List {
        #[arg(long)]
        media_id: Option<i64>,
        #[arg(long)]
        segment_id: Option<i64>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        status: Option<String>,
        #[arg(long)]
        limit: Option<i64>,
    },
    /// job.cancel
    Cancel {
        #[arg(long)]
        id: String,
    },
}

#[derive(Subcommand)]
enum SourceCommand {
    /// source.list
    List,
    /// source.search
    Search {
        #[arg(long)]
        source_id: String,
        #[arg(long)]
        query: String,
        #[arg(long)]
        limit: Option<u32>,
    },
    /// source.get_work
    GetWork {
        #[arg(long)]
        source_id: String,
        #[arg(long)]
        remote_id: String,
        /// Re-fetch instead of using the cached work.
        #[arg(long)]
        refresh: bool,
    },
}

#[derive(Subcommand)]
enum DownloadCommand {
    /// download.enqueue; omitting --asset-id fetches every asset of the work.
    Enqueue {
        #[arg(long)]
        source_id: String,
        #[arg(long)]
        remote_id: String,
        #[arg(long = "asset-id")]
        asset_ids: Vec<String>,
        #[arg(long)]
        dest_dir: PathBuf,
        /// Do not import landed audio/video into the library.
        #[arg(long)]
        no_import: bool,
    },
    /// download.list
    List {
        #[arg(long)]
        status: Option<String>,
    },
    /// download.cancel
    Cancel {
        #[arg(long)]
        id: String,
    },
    /// download.retry
    Retry {
        #[arg(long)]
        id: String,
    },
}

#[derive(Subcommand)]
enum DlsiteCommand {
    /// dlsite.session_status
    Status,
    /// dlsite.logout: deletes the stored session from the OS keyring.
    Logout,
    /// dlsite.library: the signed-in account's purchases.
    Library,
}

#[derive(Subcommand)]
enum RuntimeCommand {
    /// runtime.plan: packs a provider/model needs and their cost.
    Plan(PackArgs),
    /// runtime.install
    Install(PackArgs),
}

#[derive(Args)]
struct PackArgs {
    #[arg(long)]
    provider_id: String,
    #[arg(long)]
    model_id: Option<String>,
}

#[derive(Subcommand)]
enum MediaCommand {
    /// media.list
    List,
    /// media.import_folder: recursive; same-name srt/vtt/lrc sidecars seed segments.
    ImportFolder {
        /// Folder to import; a relative path is resolved against the current directory.
        path: PathBuf,
    },
}

#[derive(Subcommand)]
enum AsrCommand {
    /// asr.run: queues an asr job; follow it with `job list --media-id`.
    Run {
        #[arg(long)]
        media_id: i64,
        #[arg(long)]
        provider_id: String,
        #[arg(long)]
        model_id: Option<String>,
        #[arg(long)]
        language: Option<String>,
        #[arg(long)]
        prompt: Option<String>,
    },
}

#[derive(Subcommand)]
enum TranslateCommand {
    /// translate.run: queues a translate job; follow it with job list --media-id.
    Run {
        #[arg(long)]
        media_id: i64,
        #[arg(long)]
        provider_id: String,
        #[arg(long)]
        model_id: String,
        #[arg(long)]
        target_language: String,
        #[arg(long)]
        source_language: Option<String>,
        /// Only these Segments (repeat the flag); default every Segment.
        #[arg(long = "segment-id")]
        segment_ids: Vec<i64>,
        /// Also replace translations a person edited.
        #[arg(long)]
        overwrite_human_edits: bool,
    },
}

#[derive(Subcommand)]
enum TtsCommand {
    /// tts.synthesize: queues a tts job for one Segment.
    Synthesize {
        #[arg(long)]
        segment_id: i64,
        #[arg(long)]
        expected_revision: i64,
        #[arg(long)]
        voice_profile_id: Option<String>,
        #[arg(long)]
        take: Option<u32>,
    },
}

#[derive(Subcommand)]
enum SpatialCommand {
    /// spatial.analyze: queues the media's spatial analysis (cached).
    Analyze {
        #[arg(long)]
        media_id: i64,
    },
    /// spatial.render: queues one Segment's spatial render.
    Render {
        #[arg(long)]
        segment_id: i64,
        /// source_trajectory (default) or concurrent.
        #[arg(long)]
        mode: Option<String>,
    },
}

#[derive(Subcommand)]
enum MixCommand {
    /// mix.export: queues the mix/dub-stem WAV export.
    Export {
        #[arg(long)]
        media_id: i64,
        /// WAV to write; a relative path is resolved against the current directory.
        #[arg(long)]
        output: PathBuf,
        /// MixOptions overrides as a JSON object, e.g. {"kind":"dub_stem"}.
        #[arg(long)]
        options: Option<String>,
    },
}

/// What a command asks of the Desktop, decided before connecting.
enum Request {
    Status,
    Ensure,
    Init,
    ListOps,
    Op(String, Value),
}

/// Result of one CLI invocation, so tests can run it in-process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    pub code: u8,
    pub stdout: String,
    pub stderr: String,
}

/// Exit code for an error code; part of the CLI contract.
pub fn exit_code(code: &str) -> u8 {
    match code {
        "usage" => 2,
        "desktop_not_running" | "launch_failed" => 3,
        "unauthorized" => 4,
        "revision_conflict" => 5,
        "not_found" => 6,
        "invalid_input" => 7,
        "cancelled" | "superseded" => 8,
        _ => 1,
    }
}

pub fn run<I, T>(args: I) -> Outcome
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let json = args.iter().skip(1).any(|arg| arg == "--json");
    let cli = match Cli::try_parse_from(&args) {
        Ok(cli) => cli,
        Err(error) => {
            use clap::error::ErrorKind;
            let text = error.render().to_string();
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) {
                return Outcome {
                    code: 0,
                    stdout: text,
                    stderr: String::new(),
                };
            }
            return render(json, Err(ClientError::new("usage", text.trim_end())));
        }
    };
    let result = request(&cli.command).and_then(|request| execute(&cli, request));
    render(cli.json, result)
}

fn request(command: &Command) -> Result<Request, ClientError> {
    let op = |name: &str, input: Value| Ok(Request::Op(name.to_string(), input));
    match command {
        Command::Doctor => op("runtime.doctor", json!({})),
        Command::App(AppCommand::Status) => Ok(Request::Status),
        Command::App(AppCommand::Ensure) => Ok(Request::Ensure),
        Command::Init => Ok(Request::Init),
        Command::Ops(OpsCommand::List) => Ok(Request::ListOps),
        Command::Call { name, input } => {
            let input = serde_json::from_str(input).map_err(|e| {
                ClientError::new("invalid_input", format!("--input is not JSON: {e}"))
            })?;
            op(name, input)
        }
        Command::Segment(SegmentCommand::List { media_id }) => {
            op("segment.list", json!({ "media_id": media_id }))
        }
        Command::Segment(SegmentCommand::Update {
            id,
            expected_revision,
            source_text,
            translated_text,
            start_ms,
            end_ms,
        }) => op(
            "segment.update",
            object([
                ("id", Some(json!(id))),
                ("expected_revision", Some(json!(expected_revision))),
                ("source_text", source_text.as_ref().map(|v| json!(v))),
                (
                    "translated_text",
                    translated_text.as_ref().map(|v| json!(v)),
                ),
                ("start_ms", start_ms.map(|v| json!(v))),
                ("end_ms", end_ms.map(|v| json!(v))),
            ]),
        ),
        Command::Segment(SegmentCommand::Regenerate {
            segment_id,
            expected_revision,
            voice_profile_id,
            spatial_mode,
        }) => op(
            "segment.regenerate",
            object([
                ("segment_id", Some(json!(segment_id))),
                ("expected_revision", Some(json!(expected_revision))),
                (
                    "voice_profile_id",
                    voice_profile_id.as_ref().map(|v| json!(v)),
                ),
                ("spatial_mode", spatial_mode.as_ref().map(|v| json!(v))),
            ]),
        ),
        Command::Job(JobCommand::List {
            media_id,
            segment_id,
            kind,
            status,
            limit,
        }) => {
            let filter = object([
                ("media_id", media_id.map(|v| json!(v))),
                ("segment_id", segment_id.map(|v| json!(v))),
                ("kind", kind.as_ref().map(|v| json!(v))),
                ("status", status.as_ref().map(|v| json!(v))),
                ("limit", limit.map(|v| json!(v))),
            ]);
            let input = if filter.as_object().is_some_and(Map::is_empty) {
                json!({})
            } else {
                json!({ "filter": filter })
            };
            op("job.list", input)
        }
        Command::Job(JobCommand::Cancel { id }) => op("job.cancel", json!({ "id": id })),
        Command::Runtime(RuntimeCommand::Plan(args)) => op("runtime.plan", pack_input(args)),
        Command::Runtime(RuntimeCommand::Install(args)) => op("runtime.install", pack_input(args)),
        Command::Media(MediaCommand::List) => op("media.list", json!({})),
        Command::Media(MediaCommand::ImportFolder { path }) => {
            let path = std::path::absolute(path).map_err(|e| {
                ClientError::new(
                    "invalid_input",
                    format!("cannot resolve {}: {e}", path.display()),
                )
            })?;
            op(
                "media.import_folder",
                json!({ "path": path.to_string_lossy() }),
            )
        }
        Command::Asr(AsrCommand::Run {
            media_id,
            provider_id,
            model_id,
            language,
            prompt,
        }) => op(
            "asr.run",
            object([
                ("media_id", Some(json!(media_id))),
                ("provider_id", Some(json!(provider_id))),
                ("model_id", model_id.as_ref().map(|v| json!(v))),
                ("language", language.as_ref().map(|v| json!(v))),
                ("prompt", prompt.as_ref().map(|v| json!(v))),
            ]),
        ),
        Command::Translate(TranslateCommand::Run {
            media_id,
            provider_id,
            model_id,
            target_language,
            source_language,
            segment_ids,
            overwrite_human_edits,
        }) => op(
            "translate.run",
            object([
                ("media_id", Some(json!(media_id))),
                ("provider_id", Some(json!(provider_id))),
                ("model_id", Some(json!(model_id))),
                ("target_language", Some(json!(target_language))),
                (
                    "source_language",
                    source_language.as_ref().map(|v| json!(v)),
                ),
                (
                    "segment_ids",
                    (!segment_ids.is_empty()).then(|| json!(segment_ids)),
                ),
                (
                    "overwrite_human_edits",
                    overwrite_human_edits.then(|| json!(true)),
                ),
            ]),
        ),
        Command::Tts(TtsCommand::Synthesize {
            segment_id,
            expected_revision,
            voice_profile_id,
            take,
        }) => op(
            "tts.synthesize",
            object([
                ("segment_id", Some(json!(segment_id))),
                ("expected_revision", Some(json!(expected_revision))),
                (
                    "voice_profile_id",
                    voice_profile_id.as_ref().map(|v| json!(v)),
                ),
                ("take", take.map(|v| json!(v))),
            ]),
        ),
        Command::Spatial(SpatialCommand::Analyze { media_id }) => {
            op("spatial.analyze", json!({ "media_id": media_id }))
        }
        Command::Spatial(SpatialCommand::Render { segment_id, mode }) => op(
            "spatial.render",
            object([
                ("segment_id", Some(json!(segment_id))),
                ("mode", mode.as_ref().map(|v| json!(v))),
            ]),
        ),
        Command::Mix(MixCommand::Export {
            media_id,
            output,
            options,
        }) => {
            let output = std::path::absolute(output).map_err(|e| {
                ClientError::new(
                    "invalid_input",
                    format!("cannot resolve {}: {e}", output.display()),
                )
            })?;
            let options = options
                .as_deref()
                .map(serde_json::from_str::<Value>)
                .transpose()
                .map_err(|e| {
                    ClientError::new("invalid_input", format!("--options is not JSON: {e}"))
                })?;
            op(
                "mix.export",
                object([
                    ("media_id", Some(json!(media_id))),
                    ("output_path", Some(json!(output.to_string_lossy()))),
                    ("options", options),
                ]),
            )
        }
        Command::Source(SourceCommand::List) => op("source.list", json!({})),
        Command::Source(SourceCommand::Search {
            source_id,
            query,
            limit,
        }) => op(
            "source.search",
            object([
                ("source_id", Some(json!(source_id))),
                ("query", Some(json!(query))),
                ("limit", limit.map(|v| json!(v))),
            ]),
        ),
        Command::Source(SourceCommand::GetWork {
            source_id,
            remote_id,
            refresh,
        }) => op(
            "source.get_work",
            json!({ "source_id": source_id, "remote_id": remote_id, "refresh": refresh }),
        ),
        Command::Download(DownloadCommand::Enqueue {
            source_id,
            remote_id,
            asset_ids,
            dest_dir,
            no_import,
        }) => {
            let dest_dir = std::path::absolute(dest_dir).map_err(|e| {
                ClientError::new(
                    "invalid_input",
                    format!("cannot resolve {}: {e}", dest_dir.display()),
                )
            })?;
            op(
                "download.enqueue",
                object([
                    ("source_id", Some(json!(source_id))),
                    ("remote_id", Some(json!(remote_id))),
                    (
                        "asset_ids",
                        (!asset_ids.is_empty()).then(|| json!(asset_ids)),
                    ),
                    ("dest_dir", Some(json!(dest_dir.to_string_lossy()))),
                    ("import", no_import.then(|| json!(false))),
                ]),
            )
        }
        Command::Download(DownloadCommand::List { status }) => op(
            "download.list",
            object([("status", status.as_ref().map(|v| json!(v)))]),
        ),
        Command::Download(DownloadCommand::Cancel { id }) => {
            op("download.cancel", json!({ "id": id }))
        }
        Command::Download(DownloadCommand::Retry { id }) => {
            op("download.retry", json!({ "id": id }))
        }
        Command::Dlsite(DlsiteCommand::Status) => op("dlsite.session_status", json!({})),
        Command::Dlsite(DlsiteCommand::Logout) => op("dlsite.logout", json!({})),
        Command::Dlsite(DlsiteCommand::Library) => op("dlsite.library", json!({})),
    }
}

fn pack_input(args: &PackArgs) -> Value {
    object([
        ("provider_id", Some(json!(args.provider_id))),
        ("model_id", args.model_id.as_ref().map(|v| json!(v))),
    ])
}

/// A JSON object of the fields that were given; omitted flags stay omitted.
fn object<const N: usize>(fields: [(&str, Option<Value>); N]) -> Value {
    Value::Object(
        fields
            .into_iter()
            .filter_map(|(key, value)| value.map(|value| (key.to_string(), value)))
            .collect(),
    )
}

fn execute(cli: &Cli, request: Request) -> Result<Value, ClientError> {
    let opts = DiscoverOptions {
        control_file: cli.control_file.clone(),
        launch: cli.launch && !matches!(request, Request::Status),
        ..DiscoverOptions::default()
    };
    match request {
        Request::Ensure => return ensure(&opts),
        Request::Init => return init(&opts),
        _ => {}
    }
    let client = ControlClient::discover(&opts)?;
    match request {
        Request::Status => Ok(json!({
            "running": true,
            "pid": client.pid(),
            "addr": client.addr(),
            "health": client.health()?,
        })),
        Request::ListOps => serde_json::to_value(client.list_ops()?)
            .map_err(|e| ClientError::new("bad_response", e.to_string())),
        Request::Op(name, input) => client.call(&name, input),
        Request::Ensure | Request::Init => unreachable!("handled above"),
    }
}

/// app ensure: connect, and launch only when nothing answers.
fn ensure(opts: &DiscoverOptions) -> Result<Value, ClientError> {
    let probe = DiscoverOptions {
        launch: false,
        ..opts.clone()
    };
    let (client, launched) = match ControlClient::discover(&probe) {
        Ok(client) => (client, false),
        Err(error) if error.code == "desktop_not_running" => {
            let launch = DiscoverOptions {
                launch: true,
                ..opts.clone()
            };
            (ControlClient::discover(&launch)?, true)
        }
        Err(error) => return Err(error),
    };
    Ok(json!({
        "running": true,
        "launched": launched,
        "pid": client.pid(),
        "addr": client.addr(),
        "control_file": client.control_file().display().to_string(),
        "health": client.health()?,
    }))
}

/// init: what an agent needs to start driving YUZUKI.
fn init(opts: &DiscoverOptions) -> Result<Value, ClientError> {
    let exe = std::env::current_exe().ok();
    let mcp = exe
        .as_deref()
        .and_then(Path::parent)
        .map(|dir| dir.join(format!("yuzuki-mcp{}", std::env::consts::EXE_SUFFIX)))
        .filter(|path| path.is_file())
        .map(|path| path.display().to_string());
    let cli_info = json!({
        "version": env!("CARGO_PKG_VERSION"),
        "path": exe.as_ref().map(|path| path.display().to_string()),
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
    });
    let mcp_info = json!({
        "path": mcp,
        "config": mcp.as_ref().map(|command| json!({
            "mcpServers": { "yuzuki": { "command": command, "args": [] } }
        })),
    });
    match ControlClient::discover(opts) {
        Ok(client) => {
            let doctor = client.call("runtime.doctor", json!({}))?;
            let operations = client.list_ops()?.len();
            Ok(json!({
                "cli": cli_info,
                "mcp": mcp_info,
                "desktop": {
                    "running": true,
                    "pid": client.pid(),
                    "addr": client.addr(),
                    "control_file": client.control_file().display().to_string(),
                    "operations": operations,
                },
                "doctor": doctor,
                "next_steps": [
                    "yuzuki ops list --json lists every operation with its input schema",
                    "yuzuki runtime plan --provider-id <provider> --model-id <model> --json shows downloads before any install"
                ],
            }))
        }
        Err(error) if error.code == "desktop_not_running" => Ok(json!({
            "cli": cli_info,
            "mcp": mcp_info,
            "desktop": { "running": false, "reason": error.message },
            "doctor": null,
            "next_steps": [
                "Start YUZUKI, or run yuzuki app ensure --json to start it"
            ],
        })),
        Err(error) => Err(error),
    }
}

fn render(json: bool, result: Result<Value, ClientError>) -> Outcome {
    match result {
        Ok(data) => Outcome {
            code: 0,
            stdout: pretty(&if json {
                json!({ "ok": true, "data": data })
            } else {
                data
            }),
            stderr: String::new(),
        },
        Err(error) if json => Outcome {
            code: exit_code(&error.code),
            stdout: pretty(&json!({
                "ok": false,
                "error": { "code": error.code, "message": error.message },
            })),
            stderr: String::new(),
        },
        Err(error) => Outcome {
            code: exit_code(&error.code),
            stdout: String::new(),
            stderr: format!("error[{}]: {}\n", error.code, error.message),
        },
    }
}

fn pretty(value: &Value) -> String {
    let mut text = serde_json::to_string_pretty(value).expect("a JSON value serializes");
    text.push('\n');
    text
}
