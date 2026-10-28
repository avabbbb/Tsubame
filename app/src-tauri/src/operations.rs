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
    fn run(ctx: &OperationContext<'_>, _input: Self::Input) -> Result<Self::Output, OperationError> {
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
        let imported = {
            let mut db = ctx.db()?;
            let mut imported = Vec::with_capacity(files.len());
            for file in &files {
                // The library stores UTF-8 paths; a non-UTF-8 name cannot round-trip.
                let Some(file) = file.to_str() else { continue };
                let item = db.import_media(file).map_err(db_error)?;
                crate::seed_sidecar_subtitles(&mut db, item.id, Path::new(file)).map_err(db_error)?;
                imported.push(item);
            }
            imported
        };
        media_imported(&imported);
        Ok(imported)
    }
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
            return Err(OperationError::new("not_found", false, "not_found: media not found"));
        }
        let job_input = serde_json::to_value(input)
            .map_err(|e| OperationError::new("internal", false, e.to_string()))?;
        ctx.jobs
            .submit(NewJob::new(crate::ASR_JOB, media_id).input(job_input))
            .map_err(|e| OperationError::new("job_store", false, e))
    }
}

#[cfg(test)]
mod tests;
