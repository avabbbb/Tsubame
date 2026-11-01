//! Operation Registry (docs/07-agent-control.md, ADR-007).
//!
//! Every automatable product operation is one [Operation]: a stable dotted
//! name, typed input/output, and declared mutation, revision, side-effect and
//! cancellation semantics. Tauri commands are thin wrappers over [invoke];
//! the local control endpoint, CLI and MCP reach the same code through
//! [invoke_json]. There is no second business path.

use crate::asr::AsrRunInput;
use crate::db::{MediaDb, MediaItem, Segment};
use crate::jobs::{run_blocking, Job, JobError, JobFilter, JobStore, NewJob};
use crate::providers::{builtin_local_providers, ProviderConfig};
use crate::regenerate::{default_voice_profile, RegenerateInput, REGENERATE_JOB};
use crate::runtime::{
    builtin_catalog, default_probe, doctor, env_lookup, plan_packs, provider_pack_ids, root,
    InstallPlan, PackStatus, INSTALL_JOB,
};
use crate::sources::download::Download;
use crate::sources::{SearchFilters, SourceAdapter, SourceError, SourceInfo, Work};
use crate::spatial::SpatialMode;
use crate::tts::TtsError;
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Component, Path};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// What an operation may change outside its return value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SideEffect {
    /// Writes YUZUKI's product SQLite state.
    DbWrite,
    /// Creates or changes a row in the jobs table.
    JobWrite,
    /// Writes files under the app data directory (assets, caches).
    Filesystem,
    /// Starts a local worker or runtime process.
    ProcessSpawn,
    /// May call a remote provider (cost and network).
    Network,
}

/// How a caller stops an operation that is still running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Cancellation {
    /// Returns promptly; nothing to cancel.
    None,
    /// Runs as a job; job.cancel on its job id stops it cooperatively and
    /// the operation then fails with code cancelled, committing nothing.
    Job,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct OperationMeta {
    /// Stable dotted name (segment.update); part of the agent contract.
    pub name: &'static str,
    pub mutates: bool,
    /// Input carries expected_revision; a stale value fails with
    /// revision_conflict and changes nothing.
    pub expected_revision: bool,
    pub side_effects: &'static [SideEffect],
    pub cancellation: Cancellation,
}

/// Failure of an operation. code is the stable machine contract; message
/// is the human text the Desktop has always shown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OperationError {
    pub code: String,
    pub retryable: bool,
    pub message: String,
}

impl OperationError {
    pub fn new(code: &str, retryable: bool, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            retryable,
            message: message.into(),
        }
    }
}

impl From<JobError> for OperationError {
    fn from(error: JobError) -> Self {
        Self {
            code: error.code,
            retryable: error.retryable,
            message: error.message,
        }
    }
}

/// Keeps the text the Desktop showed before the registry (code: message,
/// or the Inspector's revision-conflict phrase).
impl From<TtsError> for OperationError {
    fn from(error: TtsError) -> Self {
        Self {
            message: error.to_string(),
            code: error.code,
            retryable: error.retryable,
        }
    }
}

fn db_error(error: impl ToString) -> OperationError {
    OperationError::new("db", false, error.to_string())
}

/// Desktop state an operation runs against.
pub struct OperationContext<'a> {
    pub db: &'a Mutex<MediaDb>,
    pub jobs: &'a Arc<JobStore>,
}

impl OperationContext<'_> {
    fn db(&self) -> Result<MutexGuard<'_, MediaDb>, OperationError> {
        self.db
            .lock()
            .map_err(|e| OperationError::new("state_poisoned", false, e.to_string()))
    }
}

/// One product operation. run blocks: call it from a blocking context.
pub trait Operation {
    /// JsonSchema: the input's JSON Schema is part of the agent contract
    /// (GET /v1/ops input_schema, MCP inputSchema).
    type Input: DeserializeOwned + JsonSchema;
    type Output: Serialize;
    const META: OperationMeta;
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError>;
}

/// Typed dispatch: what Tauri commands call.
pub fn invoke<O: Operation>(
    ctx: &OperationContext<'_>,
    input: O::Input,
) -> Result<O::Output, OperationError> {
    O::run(ctx, input)
}

fn invoke_value<O: Operation>(
    ctx: &OperationContext<'_>,
    input: Value,
) -> Result<Value, OperationError> {
    // An operation without arguments accepts null as well as {}.
    let input = if input.is_null() {
        Value::Object(Default::default())
    } else {
        input
    };
    let input: O::Input = serde_json::from_value(input)
        .map_err(|e| OperationError::new("invalid_input", false, e.to_string()))?;
    let output = O::run(ctx, input)?;
    serde_json::to_value(output).map_err(|e| OperationError::new("internal", false, e.to_string()))
}

pub struct Entry {
    pub meta: OperationMeta,
    invoke: fn(&OperationContext<'_>, Value) -> Result<Value, OperationError>,
    schema: fn() -> Value,
}

impl Entry {
    /// JSON Schema (draft 2020-12) of the operation's input, generated from
    /// its Input type, so it cannot drift from what dispatch accepts.
    pub fn input_schema(&self) -> Value {
        (self.schema)()
    }

    /// What GET /v1/ops lists for the operation: its metadata plus input_schema.
    pub fn describe(&self) -> Value {
        let mut value = serde_json::json!(self.meta);
        value["input_schema"] = self.input_schema();
        value
    }
}

fn input_schema_of<O: Operation>() -> Value {
    schemars::schema_for!(O::Input).to_value()
}

const fn entry<O: Operation>() -> Entry {
    Entry {
        meta: O::META,
        invoke: invoke_value::<O>,
        schema: input_schema_of::<O>,
    }
}

static REGISTRY: &[Entry] = &[
    entry::<SegmentList>(),
    entry::<SegmentUpdate>(),
    entry::<SegmentRegenerate>(),
    entry::<JobList>(),
    entry::<JobCancel>(),
    entry::<RuntimeDoctor>(),
    entry::<ProviderList>(),
    entry::<RuntimePlan>(),
    entry::<RuntimeInstall>(),
    entry::<MediaList>(),
    entry::<MediaImportFolder>(),
    entry::<AsrRun>(),
    entry::<TranslateRun>(),
    entry::<TtsSynthesize>(),
    entry::<SpatialAnalyze>(),
    entry::<SpatialRender>(),
    entry::<MixExport>(),
    entry::<SourceList>(),
    entry::<SourceSearch>(),
    entry::<SourceGetWork>(),
    entry::<DlsiteSessionStatus>(),
    entry::<DlsiteLogout>(),
    entry::<DlsiteLibrary>(),
    entry::<DownloadEnqueue>(),
    entry::<DownloadList>(),
    entry::<DownloadCancel>(),
    entry::<DownloadRetry>(),
];

/// Every registered operation, in a stable order.
pub fn registry() -> &'static [Entry] {
    REGISTRY
}

/// JSON dispatch by name: what the control endpoint, CLI and MCP call.
pub fn invoke_json(
    ctx: &OperationContext<'_>,
    name: &str,
    input: Value,
) -> Result<Value, OperationError> {
    let entry = REGISTRY
        .iter()
        .find(|entry| entry.meta.name == name)
        .ok_or_else(|| {
            OperationError::new(
                "unknown_operation",
                false,
                format!("unknown operation: {name}"),
            )
        })?;
    (entry.invoke)(ctx, input)
}

/// Input of operations that take no arguments.
#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NoInput {}

// ----- segment.list -----

pub struct SegmentList;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SegmentListInput {
    pub media_id: i64,
}

impl Operation for SegmentList {
    type Input = SegmentListInput;
    type Output = Vec<Segment>;
    const META: OperationMeta = OperationMeta {
        name: "segment.list",
        mutates: false,
        expected_revision: false,
        side_effects: &[],
        cancellation: Cancellation::None,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        ctx.db()?.get_segments(input.media_id).map_err(db_error)
    }
}

// ----- segment.update -----

pub struct SegmentUpdate;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SegmentUpdateInput {
    pub id: i64,
    pub expected_revision: i64,
    /// Omitted fields keep their stored value, so an agent can change one
    /// field without echoing the rest.
    #[serde(default)]
    pub source_text: Option<String>,
    #[serde(default)]
    pub translated_text: Option<String>,
    #[serde(default)]
    pub start_ms: Option<i64>,
    #[serde(default)]
    pub end_ms: Option<i64>,
}

impl Operation for SegmentUpdate {
    type Input = SegmentUpdateInput;
    type Output = Segment;
    const META: OperationMeta = OperationMeta {
        name: "segment.update",
        mutates: true,
        expected_revision: true,
        side_effects: &[SideEffect::DbWrite],
        cancellation: Cancellation::None,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        // A missing Segment is also a conflict: the caller's revision no
        // longer describes anything that exists.
        let conflict =
            || OperationError::new("revision_conflict", false, "segment revision conflict");
        let db = ctx.db()?;
        let current = db
            .get_segment(input.id)
            .map_err(db_error)?
            .ok_or_else(conflict)?;
        let start_ms = input.start_ms.unwrap_or(current.start_ms);
        let end_ms = input.end_ms.unwrap_or(current.end_ms);
        if start_ms < 0 {
            return Err(OperationError::new(
                "invalid_input",
                false,
                "segment start must be non-negative",
            ));
        }
        if end_ms <= start_ms {
            return Err(OperationError::new(
                "invalid_input",
                false,
                "segment end must be after start",
            ));
        }
        db.update_segment(
            input.id,
            input.expected_revision,
            input.source_text.as_deref().unwrap_or(&current.source_text),
            input
                .translated_text
                .as_deref()
                .unwrap_or(&current.translated_text),
            start_ms,
            end_ms,
        )
        .map_err(db_error)?
        .ok_or_else(conflict)
    }
}

// ----- segment.regenerate -----

pub struct SegmentRegenerate;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SegmentRegenerateInput {
    pub segment_id: i64,
    pub expected_revision: i64,
    #[serde(default)]
    pub voice_profile_id: Option<String>,
    #[serde(default)]
    pub spatial_mode: Option<SpatialMode>,
}

impl Operation for SegmentRegenerate {
    type Input = SegmentRegenerateInput;
    /// The RegenerateOutcome JSON.
    type Output = Value;
    const META: OperationMeta = OperationMeta {
        name: "segment.regenerate",
        mutates: true,
        expected_revision: true,
        side_effects: &[
            SideEffect::DbWrite,
            SideEffect::JobWrite,
            SideEffect::Filesystem,
            SideEffect::ProcessSpawn,
            SideEffect::Network,
        ],
        cancellation: Cancellation::Job,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        let (media_id, voice_profile_id, generation) = {
            let db = ctx.db()?;
            let segment = db
                .get_segment(input.segment_id)
                .map_err(db_error)?
                .ok_or_else(|| {
                    OperationError::from(TtsError::new("not_found", "segment not found", false))
                })?;
            let voice = match input.voice_profile_id.filter(|id| !id.trim().is_empty()) {
                Some(id) => id,
                None => default_voice_profile(db.connection(), input.segment_id)
                    .map_err(OperationError::from)?,
            };
            // This request supersedes any regenerate of the Segment still running.
            let generation = crate::takes::bump_generation(db.connection(), input.segment_id)
                .map_err(db_error)?;
            (segment.media_id, voice, generation)
        };
        let runner = ctx.jobs.runner(REGENERATE_JOB).ok_or_else(|| {
            OperationError::new(
                "runner_missing",
                false,
                "regenerate runner is not registered",
            )
        })?;
        let job_input = serde_json::to_value(RegenerateInput {
            segment_id: input.segment_id,
            expected_revision: input.expected_revision,
            voice_profile_id,
            generation: Some(generation),
            spatial_mode: input.spatial_mode.unwrap_or_default(),
        })
        .map_err(|e| OperationError::new("internal", false, e.to_string()))?;
        let new = NewJob::new(REGENERATE_JOB, media_id)
            .segment(input.segment_id)
            .input(job_input);
        Ok(run_blocking(ctx.jobs, new, &runner)?)
    }
}

// ----- job.list -----

pub struct JobList;

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JobListInput {
    #[serde(default)]
    pub filter: Option<JobFilter>,
}

impl Operation for JobList {
    type Input = JobListInput;
    type Output = Vec<Job>;
    const META: OperationMeta = OperationMeta {
        name: "job.list",
        mutates: false,
        expected_revision: false,
        side_effects: &[],
        cancellation: Cancellation::None,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        ctx.jobs
            .list(&input.filter.unwrap_or_default())
            .map_err(|e| OperationError::new("job_store", false, e))
    }
}

// ----- job.cancel -----

pub struct JobCancel;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JobCancelInput {
    pub id: String,
}

impl Operation for JobCancel {
    type Input = JobCancelInput;
    type Output = Job;
    const META: OperationMeta = OperationMeta {
        name: "job.cancel",
        mutates: true,
        expected_revision: false,
        side_effects: &[SideEffect::JobWrite],
        cancellation: Cancellation::None,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        ctx.jobs
            .cancel(&input.id)
            .map_err(|e| OperationError::new("job_store", false, e))
    }
}

// ----- runtime.doctor -----

pub struct RuntimeDoctor;

impl Operation for RuntimeDoctor {
    type Input = NoInput;
    type Output = Vec<PackStatus>;
    const META: OperationMeta = OperationMeta {
        name: "runtime.doctor",
        mutates: false,
        expected_revision: false,
        side_effects: &[],
        cancellation: Cancellation::None,
    };
    fn run(
        _ctx: &OperationContext<'_>,
        _input: Self::Input,
    ) -> Result<Self::Output, OperationError> {
        Ok(doctor(
            root(),
            &builtin_catalog(),
            &env_lookup,
            &default_probe,
        ))
    }
}

// ----- runtime.plan -----

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RuntimePlanInput {
    pub provider_id: String,
    #[serde(default)]
    pub model_id: Option<String>,
}

/// The packs provider_id/model_id needs and what installing them costs.
pub fn plan_install(
    provider_id: &str,
    model_id: Option<&str>,
) -> Result<InstallPlan, OperationError> {
    let model_id = model_id.unwrap_or_default();
    let ids = provider_pack_ids(provider_id, model_id).ok_or_else(|| {
        OperationError::new(
            "no_packs",
            false,
            format!("{provider_id} has no installable packs"),
        )
    })?;
    plan_packs(root(), &builtin_catalog(), &ids, provider_id, model_id)
        .map_err(|e| OperationError::new("plan_failed", false, e))
}

pub struct RuntimePlan;

impl Operation for RuntimePlan {
    type Input = RuntimePlanInput;
    type Output = InstallPlan;
    const META: OperationMeta = OperationMeta {
        name: "runtime.plan",
        mutates: false,
        expected_revision: false,
        side_effects: &[],
        cancellation: Cancellation::None,
    };
    fn run(
        _ctx: &OperationContext<'_>,
        input: Self::Input,
    ) -> Result<Self::Output, OperationError> {
        plan_install(&input.provider_id, input.model_id.as_deref())
    }
}

// ----- runtime.install -----

pub struct RuntimeInstall;

impl Operation for RuntimeInstall {
    type Input = RuntimePlanInput;
    /// The queued install job; follow it with job.list, stop it with job.cancel.
    type Output = Job;
    const META: OperationMeta = OperationMeta {
        name: "runtime.install",
        mutates: true,
        expected_revision: false,
        side_effects: &[
            SideEffect::JobWrite,
            SideEffect::Filesystem,
            SideEffect::Network,
        ],
        cancellation: Cancellation::Job,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        let plan = plan_install(&input.provider_id, input.model_id.as_deref())?;
        if let Some(blocker) = plan.blocker {
            return Err(OperationError::new("install_blocked", false, blocker));
        }
        let packs: Vec<String> = plan.items.into_iter().map(|item| item.id).collect();
        let job_input = serde_json::json!({
            "provider_id": input.provider_id,
            "model_id": input.model_id.unwrap_or_default(),
            "packs": packs,
        });
        // Runtime installs are not tied to a media item; media_id 0 by convention.
        ctx.jobs
            .submit(NewJob::new(INSTALL_JOB, 0).input(job_input))
            .map_err(|e| OperationError::new("job_store", false, e))
    }
}

// ----- provider.list -----

pub struct ProviderList;

impl Operation for ProviderList {
    type Input = NoInput;
    /// Provider configs as Settings lists them, built-in local runtimes
    /// first. secret_ref only names an OS keyring entry: no raw key is ever
    /// part of this output.
    type Output = Vec<ProviderConfig>;
    const META: OperationMeta = OperationMeta {
        name: "provider.list",
        mutates: false,
        expected_revision: false,
        side_effects: &[],
        cancellation: Cancellation::None,
    };
    fn run(
        ctx: &OperationContext<'_>,
        _input: Self::Input,
    ) -> Result<Self::Output, OperationError> {
        let mut providers = ctx.db()?.list_providers().map_err(db_error)?;
        providers.extend(builtin_local_providers());
        providers.sort_by(|a, b| {
            b.system_managed
                .cmp(&a.system_managed)
                .then_with(|| a.name.cmp(&b.name))
        });
        Ok(providers)
    }
}

// ----- media hooks -----

/// Desktop-side effects of an import that need the app handle (asset scope,
/// library refresh event). Set once in setup; absent in tests.
pub type MediaImportedHook = Box<dyn Fn(&[MediaItem]) + Send + Sync>;

static MEDIA_IMPORTED: OnceLock<MediaImportedHook> = OnceLock::new();

pub fn on_media_imported(hook: MediaImportedHook) {
    let _ = MEDIA_IMPORTED.set(hook);
}

fn media_imported(items: &[MediaItem]) {
    if let Some(hook) = MEDIA_IMPORTED.get() {
        hook(items);
    }
}

// ----- media.list -----

pub struct MediaList;

impl Operation for MediaList {
    type Input = NoInput;
    type Output = Vec<MediaItem>;
    const META: OperationMeta = OperationMeta {
        name: "media.list",
        mutates: false,
        expected_revision: false,
        side_effects: &[],
        cancellation: Cancellation::None,
    };
    fn run(
        ctx: &OperationContext<'_>,
        _input: Self::Input,
    ) -> Result<Self::Output, OperationError> {
        ctx.db()?.list_media().map_err(db_error)
    }
}

// ----- media.import_folder -----

pub struct MediaImportFolder;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MediaImportFolderInput {
    /// Absolute folder path; media files are found recursively and
    /// same-directory srt/vtt/lrc sidecars seed their transcripts.
    pub path: String,
}

impl Operation for MediaImportFolder {
    type Input = MediaImportFolderInput;
    /// The imported (or already present) media items, in path order.
    type Output = Vec<MediaItem>;
    const META: OperationMeta = OperationMeta {
        name: "media.import_folder",
        mutates: true,
        expected_revision: false,
        side_effects: &[SideEffect::DbWrite],
        cancellation: Cancellation::None,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        let dir = Path::new(&input.path);
        if !dir.is_absolute()
            || dir
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(OperationError::new(
                "invalid_input",
                false,
                format!("folder path must be absolute without '..': {}", input.path),
            ));
        }
        if !dir.is_dir() {
            return Err(OperationError::new(
                "not_found",
                false,
                format!("folder does not exist: {}", input.path),
            ));
        }
        let files = crate::collect_media_files(dir).map_err(|e| {
            OperationError::new("io", true, format!("failed to scan {}: {e}", input.path))
        })?;
        import_paths(ctx, &files)
    }
}

/// Imports media files as media.import_folder does (duration probe, sidecar
/// subtitles, import hook); landed downloads reuse it.
pub fn import_paths(
    ctx: &OperationContext<'_>,
    files: &[std::path::PathBuf],
) -> Result<Vec<MediaItem>, OperationError> {
    let imported = {
        let mut db = ctx.db()?;
        let mut imported = Vec::with_capacity(files.len());
        for file in files {
            // The library stores UTF-8 paths; a non-UTF-8 name cannot round-trip.
            let Some(file) = file.to_str() else { continue };
            let mut item = db.import_media(file).map_err(db_error)?;
            if item.duration_ms <= 0 {
                if let Some(ms) = crate::media_decode::probe_duration_ms(Path::new(file)) {
                    db.update_media_duration(item.id, ms).map_err(db_error)?;
                    item.duration_ms = ms;
                }
            }
            crate::seed_sidecar_subtitles(&mut db, item.id, Path::new(file)).map_err(db_error)?;
            imported.push(item);
        }
        imported
    };
    media_imported(&imported);
    Ok(imported)
}

// ----- asr.run -----

pub struct AsrRun;

impl Operation for AsrRun {
    type Input = AsrRunInput;
    /// The queued asr job; follow it with job.list (its result carries the
    /// segments), stop it with job.cancel. Success replaces the media's segments.
    type Output = Job;
    const META: OperationMeta = OperationMeta {
        name: "asr.run",
        mutates: true,
        expected_revision: false,
        side_effects: &[
            SideEffect::DbWrite,
            SideEffect::JobWrite,
            SideEffect::Filesystem,
            SideEffect::ProcessSpawn,
            SideEffect::Network,
        ],
        cancellation: Cancellation::Job,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        let media_id = input.media_id;
        let known = ctx
            .db()?
            .list_media()
            .map_err(db_error)?
            .iter()
            .any(|item| item.id == media_id);
        if !known {
            return Err(OperationError::new(
                "not_found",
                false,
                "not_found: media not found",
            ));
        }
        let job_input = serde_json::to_value(input)
            .map_err(|e| OperationError::new("internal", false, e.to_string()))?;
        ctx.jobs
            .submit(NewJob::new(crate::ASR_JOB, media_id).input(job_input))
            .map_err(|e| OperationError::new("job_store", false, e))
    }
}

// ----- shared helpers for job-backed ops -----

fn require_media(ctx: &OperationContext<'_>, media_id: i64) -> Result<(), OperationError> {
    let known = ctx
        .db()?
        .list_media()
        .map_err(db_error)?
        .iter()
        .any(|item| item.id == media_id);
    if known {
        Ok(())
    } else {
        Err(OperationError::new(
            "not_found",
            false,
            "not_found: media not found",
        ))
    }
}

fn segment_media(ctx: &OperationContext<'_>, segment_id: i64) -> Result<i64, OperationError> {
    ctx.db()?
        .get_segment(segment_id)
        .map_err(db_error)?
        .map(|segment| segment.media_id)
        .ok_or_else(|| OperationError::new("not_found", false, "not_found: segment not found"))
}

fn invalid_op_input(message: impl Into<String>) -> OperationError {
    OperationError::new("invalid_input", false, message)
}

fn to_job_input<T: Serialize>(value: &T) -> Result<Value, OperationError> {
    serde_json::to_value(value).map_err(|e| OperationError::new("internal", false, e.to_string()))
}

/// Queues a job on the runner the Desktop registered for its kind (the same
/// runner the Tauri command uses), returning it at once.
fn submit_job(ctx: &OperationContext<'_>, new: NewJob) -> Result<Job, OperationError> {
    ctx.jobs
        .submit(new)
        .map_err(|e| OperationError::new("job_store", false, e))
}

// ----- translate.run -----

pub struct TranslateRun;

/// The translate_segments arguments.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TranslateRunInput {
    pub media_id: i64,
    /// Segments to translate; omitted means every Segment of the media.
    #[serde(default)]
    pub segment_ids: Option<Vec<i64>>,
    /// A provider.list id whose model resolves text.translate.
    pub provider_id: String,
    pub model_id: String,
    #[serde(default)]
    pub source_language: Option<String>,
    pub target_language: String,
    /// Also replace translations a person edited (default false).
    #[serde(default)]
    pub overwrite_human_edits: bool,
}

impl Operation for TranslateRun {
    type Input = TranslateRunInput;
    /// The queued translate job; its result is the TranslateOutcome JSON
    /// (translated, unchanged, stale, protected, failed batches, segments).
    type Output = Job;
    const META: OperationMeta = OperationMeta {
        name: "translate.run",
        mutates: true,
        expected_revision: false,
        side_effects: &[
            SideEffect::DbWrite,
            SideEffect::JobWrite,
            SideEffect::Network,
        ],
        cancellation: Cancellation::Job,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        require_media(ctx, input.media_id)?;
        let job_input = to_job_input(&input)?;
        submit_job(
            ctx,
            NewJob::new(crate::TRANSLATE_JOB, input.media_id).input(job_input),
        )
    }
}

// ----- tts.synthesize -----

pub struct TtsSynthesize;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TtsSynthesizeInput {
    pub segment_id: i64,
    pub expected_revision: i64,
    /// Omitted: the default Voice Profile, picked as segment.regenerate picks it.
    #[serde(default)]
    pub voice_profile_id: Option<String>,
    /// Take number to write (default 0).
    #[serde(default)]
    pub take: Option<u32>,
}

impl Operation for TtsSynthesize {
    type Input = TtsSynthesizeInput;
    /// The queued tts job; its result is the SynthesisOutcome JSON.
    type Output = Job;
    const META: OperationMeta = OperationMeta {
        name: "tts.synthesize",
        mutates: true,
        expected_revision: true,
        side_effects: &[
            SideEffect::DbWrite,
            SideEffect::JobWrite,
            SideEffect::Filesystem,
            SideEffect::Network,
        ],
        cancellation: Cancellation::Job,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        let (media_id, voice_profile_id) = {
            let db = ctx.db()?;
            let segment = db
                .get_segment(input.segment_id)
                .map_err(db_error)?
                .ok_or_else(|| {
                    OperationError::from(TtsError::new("not_found", "segment not found", false))
                })?;
            let voice = match input.voice_profile_id.filter(|id| !id.trim().is_empty()) {
                Some(id) => id,
                None => default_voice_profile(db.connection(), input.segment_id)
                    .map_err(OperationError::from)?,
            };
            (segment.media_id, voice)
        };
        let job_input = serde_json::json!({
            "segment_id": input.segment_id,
            "expected_revision": input.expected_revision,
            "voice_profile_id": voice_profile_id,
            "take": input.take.unwrap_or(0),
        });
        submit_job(
            ctx,
            NewJob::new(crate::tts::TTS_JOB, media_id)
                .segment(input.segment_id)
                .input(job_input),
        )
    }
}

// ----- spatial.analyze -----

pub struct SpatialAnalyze;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpatialAnalyzeInput {
    pub media_id: i64,
}

impl Operation for SpatialAnalyze {
    type Input = SpatialAnalyzeInput;
    /// The queued spatial job; its result is the AnalysisOutcome JSON
    /// (cached per media, so a repeat returns the cached analysis).
    type Output = Job;
    const META: OperationMeta = OperationMeta {
        name: "spatial.analyze",
        mutates: true,
        expected_revision: false,
        side_effects: &[
            SideEffect::DbWrite,
            SideEffect::JobWrite,
            SideEffect::Filesystem,
            SideEffect::ProcessSpawn,
        ],
        cancellation: Cancellation::Job,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        require_media(ctx, input.media_id)?;
        let job_input = serde_json::json!({ "op": "analyze", "media_id": input.media_id });
        submit_job(
            ctx,
            NewJob::new(crate::spatial::SPATIAL_JOB, input.media_id).input(job_input),
        )
    }
}

// ----- spatial.render -----

pub struct SpatialRender;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SpatialRenderInput {
    pub segment_id: i64,
    /// Default source_trajectory.
    #[serde(default)]
    pub mode: Option<SpatialMode>,
}

impl Operation for SpatialRender {
    type Input = SpatialRenderInput;
    /// The queued spatial job; its result is the RenderOutcome JSON. Runs the
    /// media analysis first when it is not cached.
    type Output = Job;
    const META: OperationMeta = OperationMeta {
        name: "spatial.render",
        mutates: true,
        expected_revision: false,
        side_effects: &[
            SideEffect::DbWrite,
            SideEffect::JobWrite,
            SideEffect::Filesystem,
            SideEffect::ProcessSpawn,
        ],
        cancellation: Cancellation::Job,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        let media_id = segment_media(ctx, input.segment_id)?;
        let job_input = serde_json::json!({
            "op": "render",
            "segment_id": input.segment_id,
            "mode": input.mode.unwrap_or_default(),
        });
        submit_job(
            ctx,
            NewJob::new(crate::spatial::SPATIAL_JOB, media_id)
                .segment(input.segment_id)
                .input(job_input),
        )
    }
}

// ----- mix.export -----

pub struct MixExport;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct MixExportInput {
    pub media_id: i64,
    /// Absolute path of the WAV to write, without '..'; its folder must exist.
    pub output_path: String,
    /// MixOptions fields to override (kind, limiter, dub_gain_db, use_spatial,
    /// spatial_mode, ...); omitted fields keep the Desktop export defaults.
    #[serde(default)]
    pub options: Option<Value>,
}

impl Operation for MixExport {
    type Input = MixExportInput;
    /// The queued mix job; its result is the export report (loudness, peaks,
    /// clips used, spatial clips).
    type Output = Job;
    const META: OperationMeta = OperationMeta {
        name: "mix.export",
        mutates: true,
        expected_revision: false,
        side_effects: &[SideEffect::JobWrite, SideEffect::Filesystem],
        cancellation: Cancellation::Job,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        require_media(ctx, input.media_id)?;
        let out = Path::new(&input.output_path);
        if !out.is_absolute()
            || out
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(invalid_op_input(format!(
                "output_path must be absolute without '..': {}",
                input.output_path
            )));
        }
        if !out.parent().is_some_and(Path::is_dir) {
            return Err(OperationError::new(
                "not_found",
                false,
                format!("output folder does not exist: {}", input.output_path),
            ));
        }
        let options: crate::mix::MixOptions = match input.options {
            Some(value) => serde_json::from_value(value)
                .map_err(|e| invalid_op_input(format!("invalid mix options: {e}")))?,
            None => crate::mix::MixOptions::default(),
        };
        options.validate().map_err(invalid_op_input)?;
        let job_input = serde_json::json!({
            "media_id": input.media_id,
            "output_path": input.output_path,
            "options": options,
        });
        submit_job(
            ctx,
            NewJob::new(crate::MIX_JOB, input.media_id).input(job_input),
        )
    }
}

// ----- source.* (PR #14, docs/08-source-adapters.md) -----

fn source_error(error: SourceError) -> OperationError {
    OperationError::new(&error.code, error.retryable, error.message)
}

fn find_adapter(source_id: &str) -> Result<Box<dyn SourceAdapter>, OperationError> {
    crate::sources::adapter(source_id).ok_or_else(|| {
        OperationError::new("not_found", false, format!("unknown source: {source_id}"))
    })
}

pub struct SourceList;

impl Operation for SourceList {
    type Input = NoInput;
    /// Implemented, lawful sources with their capabilities.
    type Output = Vec<SourceInfo>;
    const META: OperationMeta = OperationMeta {
        name: "source.list",
        mutates: false,
        expected_revision: false,
        side_effects: &[],
        cancellation: Cancellation::None,
    };
    fn run(
        _ctx: &OperationContext<'_>,
        _input: Self::Input,
    ) -> Result<Self::Output, OperationError> {
        Ok(crate::sources::list())
    }
}

pub struct SourceSearch;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceSearchInput {
    /// A source.list id.
    pub source_id: String,
    pub query: String,
    #[serde(default)]
    pub limit: Option<u32>,
}

impl Operation for SourceSearch {
    type Input = SourceSearchInput;
    /// Matching works; each is cached for source.get_work and download.enqueue.
    type Output = Vec<Work>;
    const META: OperationMeta = OperationMeta {
        name: "source.search",
        mutates: true,
        expected_revision: false,
        side_effects: &[SideEffect::DbWrite, SideEffect::Network],
        cancellation: Cancellation::None,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        let adapter = find_adapter(&input.source_id)?;
        let filters = SearchFilters { limit: input.limit };
        // The adapter may call the network: never hold the DB lock across it.
        let works = adapter
            .search(&input.query, &filters)
            .map_err(source_error)?;
        let db = ctx.db()?;
        for work in &works {
            // Never overwrite a fuller cached work with a list-only hit.
            crate::sources::cache_search_hit(db.connection(), work).map_err(source_error)?;
        }
        Ok(works)
    }
}

pub struct SourceGetWork;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceGetWorkInput {
    /// A source.list id.
    pub source_id: String,
    /// The source's own id (an absolute path for local, an RJ code for DLsite).
    pub remote_id: String,
    /// Ask the source again instead of returning the cached work.
    #[serde(default)]
    pub refresh: bool,
}

impl Operation for SourceGetWork {
    type Input = SourceGetWorkInput;
    type Output = Work;
    const META: OperationMeta = OperationMeta {
        name: "source.get_work",
        mutates: true,
        expected_revision: false,
        side_effects: &[SideEffect::DbWrite, SideEffect::Network],
        cancellation: Cancellation::None,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        let adapter = find_adapter(&input.source_id)?;
        if !input.refresh {
            // Search hits are cached partial: only a full fetch is returned as-is.
            let db = ctx.db()?;
            let conn = db.connection();
            let cached = crate::sources::get_work(conn, &input.source_id, &input.remote_id)
                .map_err(source_error)?;
            let partial = crate::sources::is_partial(conn, &input.source_id, &input.remote_id)
                .map_err(source_error)?;
            if let Some(work) = cached.filter(|_| !partial) {
                return Ok(work);
            }
        }
        let work = adapter.get_work(&input.remote_id).map_err(source_error)?;
        crate::sources::upsert_work(ctx.db()?.connection(), &work).map_err(source_error)?;
        Ok(work)
    }
}

// ----- dlsite ops (docs/08 "DLsite account") -----
// Sign-in itself is the app's login window (dlsite_login_open/finish): the
// password stays on DLsite's page, so CLI and MCP can only read or end it.

pub struct DlsiteSessionStatus;

impl Operation for DlsiteSessionStatus {
    type Input = NoInput;
    /// Whether a DLsite session is stored in the OS keyring (never its value).
    type Output = crate::sources::dlsite_auth::SessionStatus;
    const META: OperationMeta = OperationMeta {
        name: "dlsite.session_status",
        mutates: false,
        expected_revision: false,
        side_effects: &[],
        cancellation: Cancellation::None,
    };
    fn run(
        _ctx: &OperationContext<'_>,
        _input: Self::Input,
    ) -> Result<Self::Output, OperationError> {
        Ok(crate::sources::dlsite_auth::status())
    }
}

pub struct DlsiteLogout;

impl Operation for DlsiteLogout {
    type Input = NoInput;
    type Output = crate::sources::dlsite_auth::SessionStatus;
    const META: OperationMeta = OperationMeta {
        name: "dlsite.logout",
        mutates: true,
        expected_revision: false,
        side_effects: &[],
        cancellation: Cancellation::None,
    };
    fn run(
        _ctx: &OperationContext<'_>,
        _input: Self::Input,
    ) -> Result<Self::Output, OperationError> {
        crate::sources::dlsite_auth::clear_session();
        Ok(crate::sources::dlsite_auth::status())
    }
}

pub struct DlsiteLibrary;

impl Operation for DlsiteLibrary {
    type Input = NoInput;
    /// RJ codes the signed-in account bought; source.get_work fills metadata.
    type Output = Vec<crate::sources::dlsite_auth::OwnedWork>;
    const META: OperationMeta = OperationMeta {
        name: "dlsite.library",
        mutates: false,
        expected_revision: false,
        side_effects: &[SideEffect::Network],
        cancellation: Cancellation::None,
    };
    fn run(
        _ctx: &OperationContext<'_>,
        _input: Self::Input,
    ) -> Result<Self::Output, OperationError> {
        crate::sources::dlsite_auth::purchased_works().map_err(source_error)
    }
}

// ----- download.* (PR #14, docs/08 "Download manager") -----

fn queue_error(error: String) -> OperationError {
    OperationError::new("job_store", false, error)
}

/// What download.enqueue and download.retry return.
#[derive(Debug, Clone, Serialize)]
pub struct DownloadQueued {
    pub downloads: Vec<Download>,
    /// The download job draining the queue; follow it with job.list.
    pub job: Job,
}

fn import_by_default() -> bool {
    true
}

/// A download url for an asset location: urls as they are, local absolute
/// paths as file: urls.
fn asset_url(location: &str) -> String {
    if ["http://", "https://", "file:"]
        .iter()
        .any(|scheme| location.starts_with(scheme))
    {
        location.to_string()
    } else {
        format!(
            "file:///{}",
            location.replace('\\', "/").trim_start_matches('/')
        )
    }
}

/// Where an asset lands under dest_dir: only plain name components survive.
fn asset_dest(dest_dir: &Path, name: &str) -> Result<std::path::PathBuf, OperationError> {
    let relative: std::path::PathBuf = Path::new(name)
        .components()
        .filter_map(|component| match component {
            Component::Normal(part) => Some(part),
            _ => None,
        })
        .collect();
    if relative.as_os_str().is_empty() {
        return Err(invalid_op_input(format!(
            "asset name has no file name: {name}"
        )));
    }
    Ok(dest_dir.join(relative))
}

pub struct DownloadEnqueue;

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DownloadEnqueueInput {
    /// A source.list id.
    pub source_id: String,
    /// The work's id in that source (see source.get_work).
    pub remote_id: String,
    /// Asset ids of the work to fetch; omitted means every asset.
    #[serde(default)]
    pub asset_ids: Option<Vec<String>>,
    /// Absolute folder without '..'; each asset lands at its name under it.
    pub dest_dir: String,
    /// Import landed audio/video into the library (default true).
    #[serde(default = "import_by_default")]
    pub import: bool,
}

impl Operation for DownloadEnqueue {
    type Input = DownloadEnqueueInput;
    type Output = DownloadQueued;
    const META: OperationMeta = OperationMeta {
        name: "download.enqueue",
        mutates: true,
        expected_revision: false,
        side_effects: &[
            SideEffect::DbWrite,
            SideEffect::JobWrite,
            SideEffect::Filesystem,
            SideEffect::Network,
        ],
        cancellation: Cancellation::Job,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        let dest_dir = Path::new(&input.dest_dir);
        if !dest_dir.is_absolute()
            || dest_dir
                .components()
                .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(invalid_op_input(format!(
                "dest_dir must be absolute without '..': {}",
                input.dest_dir
            )));
        }
        let work = SourceGetWork::run(
            ctx,
            SourceGetWorkInput {
                source_id: input.source_id.clone(),
                remote_id: input.remote_id.clone(),
                refresh: false,
            },
        )?;
        let mut work = work;
        let wants_owned = input.asset_ids.as_ref().is_some_and(|ids| {
            ids.iter()
                .any(|id| id == crate::sources::dlsite_auth::OWNED_ARCHIVE_ID)
        });
        if wants_owned && work.source_id == "dlsite" {
            let rj = crate::sources::dlsite::normalize_rj(&work.remote_id).map_err(source_error)?;
            work.assets
                .push(crate::sources::dlsite_auth::owned_archive(&rj).map_err(source_error)?);
        }
        let assets = match &input.asset_ids {
            None => work.assets.iter().collect::<Vec<_>>(),
            Some(ids) => ids
                .iter()
                .map(|id| {
                    work.assets
                        .iter()
                        .find(|asset| &asset.id == id)
                        .ok_or_else(|| {
                            OperationError::new(
                                "not_found",
                                false,
                                format!("work {} has no asset {id}", work.remote_id),
                            )
                        })
                })
                .collect::<Result<Vec<_>, _>>()?,
        };
        if assets.is_empty() {
            return Err(invalid_op_input("no assets to download"));
        }
        let downloads = {
            let db = ctx.db()?;
            let conn = db.connection();
            let mut downloads = Vec::with_capacity(assets.len());
            for asset in assets {
                let dest = asset_dest(dest_dir, &asset.name)?;
                let queued = crate::sources::download::enqueue(
                    conn,
                    &asset_url(&asset.location),
                    &dest,
                    asset.size,
                    asset.sha256.as_deref(),
                )
                .map_err(source_error)?;
                let origin = crate::sources::download::Origin {
                    source_id: &work.source_id,
                    remote_id: &work.remote_id,
                    asset_id: &asset.id,
                    import: input.import,
                };
                downloads.push(
                    crate::sources::download::set_origin(conn, &queued.id, &origin)
                        .map_err(source_error)?,
                );
            }
            downloads
        };
        let job = crate::sources::download::ensure_drain(ctx.jobs).map_err(queue_error)?;
        Ok(DownloadQueued { downloads, job })
    }
}

pub struct DownloadList;

#[derive(Debug, Clone, Default, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DownloadListInput {
    /// queued | running | succeeded | failed | cancelled; omitted means all.
    #[serde(default)]
    pub status: Option<String>,
}

impl Operation for DownloadList {
    type Input = DownloadListInput;
    /// Rows in queue order with bytes, attempts, error and provenance.
    type Output = Vec<Download>;
    const META: OperationMeta = OperationMeta {
        name: "download.list",
        mutates: false,
        expected_revision: false,
        side_effects: &[],
        cancellation: Cancellation::None,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        crate::sources::download::list(ctx.db()?.connection(), input.status.as_deref())
            .map_err(source_error)
    }
}

#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DownloadIdInput {
    /// A download.list id.
    pub id: String,
}

pub struct DownloadCancel;

impl Operation for DownloadCancel {
    type Input = DownloadIdInput;
    /// The row: cancelled at once when queued; a running row is stopped by
    /// the drain and then reads cancelled in download.list.
    type Output = Download;
    const META: OperationMeta = OperationMeta {
        name: "download.cancel",
        mutates: true,
        expected_revision: false,
        side_effects: &[SideEffect::DbWrite],
        cancellation: Cancellation::None,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        let row = crate::sources::download::cancel(ctx.db()?.connection(), &input.id)
            .map_err(source_error)?;
        if row.status == "running" {
            crate::sources::download::request_stop(&row.id);
        }
        Ok(row)
    }
}

pub struct DownloadRetry;

impl Operation for DownloadRetry {
    type Input = DownloadIdInput;
    /// The re-queued row (its .part is resumed) and the draining job.
    type Output = DownloadQueued;
    const META: OperationMeta = OperationMeta {
        name: "download.retry",
        mutates: true,
        expected_revision: false,
        side_effects: &[
            SideEffect::DbWrite,
            SideEffect::JobWrite,
            SideEffect::Filesystem,
            SideEffect::Network,
        ],
        cancellation: Cancellation::Job,
    };
    fn run(ctx: &OperationContext<'_>, input: Self::Input) -> Result<Self::Output, OperationError> {
        let row = crate::sources::download::retry(ctx.db()?.connection(), &input.id)
            .map_err(source_error)?;
        let job = crate::sources::download::ensure_drain(ctx.jobs).map_err(queue_error)?;
        Ok(DownloadQueued {
            downloads: vec![row],
            job,
        })
    }
}

#[cfg(test)]
mod tests;
