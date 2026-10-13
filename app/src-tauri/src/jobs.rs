//! Jobs: persistent, observable, cancellable units of background work (PR #10).
//!
//! Contract: `docs/19-dubbing-pipeline.md` § "Jobs". Rust owns the `jobs` table;
//! workers never write it.
//!
//! # API for pipeline stages
//!
//! Every stage — in-process Rust (translate, tts, mix) or an external worker
//! (spatialize) — reports through the same table and the same `job://update`
//! event. Register a runner for a job `kind` once at startup, then submit jobs:
//!
//! ```ignore
//! use crate::jobs::{JobError, JobHandle, JobStore, NewJob};
//!
//! store.register_runner("text.translate", std::sync::Arc::new(|job: &JobHandle| {
//!     let input = job.input();                    // the job's input_json
//!     for (i, batch) in batches.iter().enumerate() {
//!         if job.is_cancelled() {
//!             return Err(JobError::cancelled());  // → status `cancelled`
//!         }
//!         job.progress(i as i64, batches.len() as i64, "translate");
//!         // ... do work; on a retryable failure:
//!         // return Err(JobError::new("rate_limited", true, "HTTP 429"));
//!     }
//!     Ok(serde_json::json!({"translated": 12}))   // → `succeeded`, output_json
//! }));
//! let job = store.submit(NewJob::new("text.translate", media_id).input(json))?;
//! ```
//!
//! A worker-backed runner forwards protocol progress and cancel for free:
//! `job.run_worker(&spec, op, input, input_files, out_dir)`.
//!
//! Lower-level control (when the caller drives the lifecycle itself):
//! `store.create(new)` → queued; `store.start(id)` → running + `JobHandle`;
//! `handle.progress(done, total, stage)`, `handle.is_cancelled()`,
//! `handle.succeed(output)`, `handle.fail(code, retryable, message)`,
//! `handle.cancelled()`. A `JobHandle` dropped while its job is still `running`
//! fails the job with `job_abandoned` (retryable). A runner that panics fails it
//! with `panic` (not retryable).
//!
//! # State machine
//!
//! ```text
//! queued ──start──▶ running ──succeed──▶ succeeded
//!   │                 ├──fail──────────▶ failed ──retry (retryable only)──▶ queued (attempt+1)
//!   └──cancel──▶ cancelled ◀──cancel ack─┘
//! ```
//!
//! `cancel_job` on a queued job cancels immediately; on a running job it sets the
//! cancel flag and the row flips to `cancelled` when the runner stops (a runner
//! that still completes successfully stays `succeeded`). A `running` row with no
//! live runner (e.g. after a crash) is cancelled immediately. On startup every
//! `queued`/`running` row is failed with `interrupted` (retryable).
//! `retry_job` accepts only `failed` + `retryable` and requires a registered
//! runner for the job kind; it clears error/output/progress and bumps `attempt`.

// Public API consumed by the other PR #10 tracks; unused until they land.
#![allow(dead_code)]

use crate::worker::{self, WorkerEvent, WorkerOutput, WorkerRequest, WorkerSpec};
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::thread;

pub const JOB_UPDATE_EVENT: &str = "job://update";
const DEFAULT_LIST_LIMIT: i64 = 200;
const NOW: &str = "strftime('%Y-%m-%dT%H:%M:%fZ','now')";

/// Creates the `jobs` table and indexes. Idempotent; called from `MediaDb::open`
/// and `JobStore::open`.
pub fn migrate(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS jobs (
           id TEXT PRIMARY KEY,
           kind TEXT NOT NULL,
           media_id INTEGER NOT NULL,
           segment_id INTEGER,
           status TEXT NOT NULL DEFAULT 'queued'
             CHECK (status IN ('queued','running','succeeded','failed','cancelled')),
           progress_done INTEGER NOT NULL DEFAULT 0,
           progress_total INTEGER NOT NULL DEFAULT 0,
           stage TEXT NOT NULL DEFAULT '',
           error_code TEXT,
           error_message TEXT,
           retryable INTEGER NOT NULL DEFAULT 0,
           attempt INTEGER NOT NULL DEFAULT 1,
           input_json TEXT NOT NULL DEFAULT '{}',
           output_json TEXT,
           created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')),
           updated_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now'))
         );
         CREATE INDEX IF NOT EXISTS idx_jobs_media ON jobs(media_id, created_at);
         CREATE INDEX IF NOT EXISTS idx_jobs_status ON jobs(status);",
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl JobStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "queued" => Self::Queued,
            "running" => Self::Running,
            "succeeded" => Self::Succeeded,
            "failed" => Self::Failed,
            "cancelled" => Self::Cancelled,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

/// One `jobs` row; also the payload of the `job://update` event.
/// `input_json` / `output_json` are stored as TEXT and carried as JSON values.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Job {
    pub id: String,
    pub kind: String,
    pub media_id: i64,
    pub segment_id: Option<i64>,
    pub status: JobStatus,
    pub progress_done: i64,
    pub progress_total: i64,
    pub stage: String,
    pub error_code: Option<String>,
    pub error_message: Option<String>,
    pub retryable: bool,
    pub attempt: i64,
    pub input_json: Value,
    pub output_json: Option<Value>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewJob {
    pub kind: String,
    pub media_id: i64,
    pub segment_id: Option<i64>,
    pub input: Value,
}

impl NewJob {
    pub fn new(kind: impl Into<String>, media_id: i64) -> Self {
        Self {
            kind: kind.into(),
            media_id,
            segment_id: None,
            input: Value::Object(Default::default()),
        }
    }

    pub fn segment(mut self, segment_id: i64) -> Self {
        self.segment_id = Some(segment_id);
        self
    }

    pub fn input(mut self, input: Value) -> Self {
        self.input = input;
        self
    }
}

/// Failure reported by a runner.
#[derive(Debug, Clone, PartialEq)]
pub struct JobError {
    pub code: String,
    pub retryable: bool,
    pub message: String,
}

impl JobError {
    pub fn new(code: impl Into<String>, retryable: bool, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            retryable,
            message: message.into(),
        }
    }

    pub fn cancelled() -> Self {
        Self::new("cancelled", true, "cancelled")
    }
}

impl From<worker::WorkerError> for JobError {
    fn from(error: worker::WorkerError) -> Self {
        Self::new(error.code, error.retryable, error.message)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct JobFilter {
    pub media_id: Option<i64>,
    pub segment_id: Option<i64>,
    pub kind: Option<String>,
    pub status: Option<String>,
    pub limit: Option<i64>,
}

pub type JobListener = Arc<dyn Fn(&Job) + Send + Sync>;
pub type JobRunner = Arc<dyn Fn(&JobHandle) -> Result<Value, JobError> + Send + Sync>;

pub struct JobStore {
    conn: Mutex<Connection>,
    listener: RwLock<Option<JobListener>>,
    runners: RwLock<HashMap<String, JobRunner>>,
    active: Mutex<HashMap<String, Arc<AtomicBool>>>,
}

fn sql_err(error: rusqlite::Error) -> String {
    error.to_string()
}

impl JobStore {
    /// Opens its own connection to the product database, migrates, and fails
    /// rows left `queued`/`running` by a previous process with `interrupted`.
    pub fn open(path: &Path) -> rusqlite::Result<Arc<Self>> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Self::from_connection(conn)
    }

    pub fn open_in_memory() -> rusqlite::Result<Arc<Self>> {
        Self::from_connection(Connection::open_in_memory()?)
    }

    pub fn from_connection(conn: Connection) -> rusqlite::Result<Arc<Self>> {
        migrate(&conn)?;
        conn.execute(
            &format!(
                "UPDATE jobs SET status = 'failed', error_code = 'interrupted',
                   error_message = 'Tsubame stopped before this job finished',
                   retryable = 1, updated_at = {NOW}
                 WHERE status IN ('queued','running')"
            ),
            [],
        )?;
        Ok(Arc::new(Self {
            conn: Mutex::new(conn),
            listener: RwLock::new(None),
            runners: RwLock::new(HashMap::new()),
            active: Mutex::new(HashMap::new()),
        }))
    }

    /// Called with the fresh row after every state/progress change.
    pub fn set_listener(&self, listener: JobListener) {
        if let Ok(mut slot) = self.listener.write() {
            *slot = Some(listener);
        }
    }

    pub fn register_runner(&self, kind: &str, runner: JobRunner) {
        if let Ok(mut runners) = self.runners.write() {
            runners.insert(kind.to_string(), runner);
        }
    }

    pub fn has_runner(&self, kind: &str) -> bool {
        self.runners
            .read()
            .map(|runners| runners.contains_key(kind))
            .unwrap_or(false)
    }

    fn emit(&self, job: &Job) {
        let listener = self.listener.read().ok().and_then(|slot| slot.clone());
        if let Some(listener) = listener {
            listener(job);
        }
    }

    /// Runs one UPDATE; returns the fresh row and emits it when a row changed.
    fn update(
        &self,
        id: &str,
        sql: &str,
        args: &[&dyn rusqlite::ToSql],
    ) -> Result<Option<Job>, String> {
        let job = {
            let conn = self.conn.lock().map_err(|e| e.to_string())?;
            let changed = conn.execute(sql, args).map_err(sql_err)?;
            if changed == 0 {
                return Ok(None);
            }
            get_job(&conn, id).map_err(sql_err)?
        };
        if let Some(job) = &job {
            self.emit(job);
        }
        Ok(job)
    }

    pub fn get(&self, id: &str) -> Result<Option<Job>, String> {
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        get_job(&conn, id).map_err(sql_err)
    }

    pub fn list(&self, filter: &JobFilter) -> Result<Vec<Job>, String> {
        if let Some(status) = &filter.status {
            if JobStatus::parse(status).is_none() {
                return Err(format!("unknown job status: {status}"));
            }
        }
        let conn = self.conn.lock().map_err(|e| e.to_string())?;
        let mut stmt = conn
            .prepare(
                "SELECT * FROM jobs
                 WHERE (?1 IS NULL OR media_id = ?1)
                   AND (?2 IS NULL OR segment_id = ?2)
                   AND (?3 IS NULL OR kind = ?3)
                   AND (?4 IS NULL OR status = ?4)
                 ORDER BY created_at DESC, rowid DESC
                 LIMIT ?5",
            )
            .map_err(sql_err)?;
        let limit = filter.limit.unwrap_or(DEFAULT_LIST_LIMIT).clamp(1, 1000);
        let rows = stmt
            .query_map(
                params![
                    filter.media_id,
                    filter.segment_id,
                    filter.kind,
                    filter.status,
                    limit
                ],
                row_to_job,
            )
            .map_err(sql_err)?
            .collect::<Result<Vec<_>, _>>()
            .map_err(sql_err)?;
        Ok(rows)
    }

    /// Inserts a `queued` job. Does not run it (see `submit`).
    pub fn create(&self, new: NewJob) -> Result<Job, String> {
        if new.kind.trim().is_empty() {
            return Err("job kind is required".into());
        }
        let id = uuid::Uuid::new_v4().to_string();
        let input = serde_json::to_string(&new.input).map_err(|e| e.to_string())?;
        let job = {
            let conn = self.conn.lock().map_err(|e| e.to_string())?;
            conn.execute(
                "INSERT INTO jobs (id, kind, media_id, segment_id, status, input_json)
                 VALUES (?1, ?2, ?3, ?4, 'queued', ?5)",
                params![id, new.kind, new.media_id, new.segment_id, input],
            )
            .map_err(sql_err)?;
            get_job(&conn, &id)
                .map_err(sql_err)?
                .ok_or_else(|| "job vanished after insert".to_string())?
        };
        self.emit(&job);
        Ok(job)
    }

    /// `create` + run the registered runner for `kind` on a background thread.
    pub fn submit(self: &Arc<Self>, new: NewJob) -> Result<Job, String> {
        if !self.has_runner(&new.kind) {
            return Err(format!("no runner registered for job kind {}", new.kind));
        }
        let job = self.create(new)?;
        self.dispatch(&job.id)?;
        Ok(job)
    }

    /// Runs a queued job with its registered runner on a background thread.
    pub fn dispatch(self: &Arc<Self>, id: &str) -> Result<(), String> {
        let job = self.get(id)?.ok_or_else(|| "job not found".to_string())?;
        let runner = self
            .runners
            .read()
            .map_err(|e| e.to_string())?
            .get(&job.kind)
            .cloned()
            .ok_or_else(|| format!("no runner registered for job kind {}", job.kind))?;
        let store = Arc::clone(self);
        let id = id.to_string();
        thread::Builder::new()
            .name(format!("job-{}", job.kind))
            .spawn(move || {
                let _ = store.execute(&id, runner.as_ref());
            })
            .map_err(|e| e.to_string())?;
        Ok(())
    }

    /// Runs a queued job on the calling thread: start → runner → terminal state.
    pub fn execute(
        self: &Arc<Self>,
        id: &str,
        runner: &(dyn Fn(&JobHandle) -> Result<Value, JobError> + Send + Sync),
    ) -> Result<Job, String> {
        let handle = self.start(id)?;
        let outcome = catch_unwind(AssertUnwindSafe(|| runner(&handle)));
        match outcome {
            Ok(Ok(output)) => {
                handle.succeed(output)?;
            }
            Ok(Err(error)) => {
                if handle.is_cancelled() || error.code == "cancelled" {
                    handle.cancelled()?;
                } else {
                    handle.fail(&error.code, error.retryable, &error.message)?;
                }
            }
            Err(panic) => {
                let message = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "runner panicked".to_string());
                handle.fail("panic", false, &message)?;
            }
        }
        drop(handle);
        self.get(id)?.ok_or_else(|| "job not found".to_string())
    }

    /// `queued → running`. The returned handle is the only writer of progress
    /// and of the terminal state.
    pub fn start(self: &Arc<Self>, id: &str) -> Result<JobHandle, String> {
        let cancel = Arc::new(AtomicBool::new(false));
        // Register the flag before the row flips so a racing cancel_job sees it.
        self.active
            .lock()
            .map_err(|e| e.to_string())?
            .insert(id.to_string(), Arc::clone(&cancel));
        let started = self.update(
            id,
            &format!("UPDATE jobs SET status = 'running', updated_at = {NOW} WHERE id = ?1 AND status = 'queued'"),
            &[&id],
        );
        match started {
            Ok(Some(_)) => Ok(JobHandle {
                store: Arc::clone(self),
                id: id.to_string(),
                cancel,
            }),
            other => {
                self.forget_active(id);
                other?;
                let status = self
                    .get(id)?
                    .map(|job| job.status.as_str().to_string())
                    .unwrap_or_else(|| "missing".into());
                Err(format!("job cannot start from status {status}"))
            }
        }
    }

    fn forget_active(&self, id: &str) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(id);
        }
    }

    fn active_flag(&self, id: &str) -> Option<Arc<AtomicBool>> {
        self.active
            .lock()
            .ok()
            .and_then(|active| active.get(id).cloned())
    }

    /// Cancels a job (see module docs). Returns the current row.
    pub fn cancel(&self, id: &str) -> Result<Job, String> {
        let job = self.get(id)?.ok_or_else(|| "job not found".to_string())?;
        match job.status {
            JobStatus::Queued => self.mark_cancelled(id, "queued").and_then(|row| match row {
                Some(job) => Ok(job),
                None => self.cancel(id), // raced with start; retry against the new state
            }),
            JobStatus::Running => {
                if let Some(flag) = self.active_flag(id) {
                    flag.store(true, Ordering::SeqCst);
                    Ok(job)
                } else {
                    // Orphaned row (no live runner): nothing to wait for.
                    Ok(self.mark_cancelled(id, "running")?.unwrap_or(job))
                }
            }
            status => Err(format!("job is already {}", status.as_str())),
        }
    }

    fn mark_cancelled(&self, id: &str, from: &str) -> Result<Option<Job>, String> {
        self.update(
            id,
            &format!(
                "UPDATE jobs SET status = 'cancelled', error_code = 'cancelled',
                   error_message = 'cancelled by user', retryable = 0, updated_at = {NOW}
                 WHERE id = ?1 AND status = ?2"
            ),
            &[&id, &from],
        )
    }

    /// `failed` + `retryable` → `queued`, attempt + 1, then dispatches the
    /// registered runner.
    pub fn retry(self: &Arc<Self>, id: &str) -> Result<Job, String> {
        let job = self.get(id)?.ok_or_else(|| "job not found".to_string())?;
        if job.status != JobStatus::Failed {
            return Err(format!(
                "only failed jobs can be retried (job is {})",
                job.status.as_str()
            ));
        }
        if !job.retryable {
            return Err(format!(
                "job failed with a non-retryable error ({})",
                job.error_code.as_deref().unwrap_or("unknown")
            ));
        }
        if !self.has_runner(&job.kind) {
            return Err(format!("no runner registered for job kind {}", job.kind));
        }
        let queued = self
            .update(
                id,
                &format!(
                    "UPDATE jobs SET status = 'queued', attempt = attempt + 1,
                       error_code = NULL, error_message = NULL, retryable = 0,
                       output_json = NULL, progress_done = 0, progress_total = 0,
                       stage = '', updated_at = {NOW}
                     WHERE id = ?1 AND status = 'failed' AND retryable = 1"
                ),
                &[&id],
            )?
            .ok_or_else(|| "job changed while retrying".to_string())?;
        self.dispatch(id)?;
        Ok(queued)
    }

    fn finish(
        &self,
        id: &str,
        status: JobStatus,
        output: Option<&Value>,
        error: Option<(&str, bool, &str)>,
    ) -> Result<Job, String> {
        let output = output
            .map(serde_json::to_string)
            .transpose()
            .map_err(|e| e.to_string())?;
        let (code, retryable, message) = match error {
            Some((code, retryable, message)) => (Some(code), retryable, Some(message)),
            None => (None, false, None),
        };
        let row = self.update(
            id,
            &format!(
                "UPDATE jobs SET status = ?2, output_json = ?3, error_code = ?4,
                   retryable = ?5, error_message = ?6,
                   progress_done = CASE WHEN ?2 = 'succeeded' AND progress_total > 0
                                        THEN progress_total ELSE progress_done END,
                   updated_at = {NOW}
                 WHERE id = ?1 AND status = 'running'"
            ),
            &[&id, &status.as_str(), &output, &code, &retryable, &message],
        )?;
        self.forget_active(id);
        match row {
            Some(job) => Ok(job),
            None => Err("job is not running".into()),
        }
    }
}

/// The writer side of one running job. See module docs.
pub struct JobHandle {
    store: Arc<JobStore>,
    id: String,
    cancel: Arc<AtomicBool>,
}

impl JobHandle {
    pub fn id(&self) -> &str {
        &self.id
    }

    pub fn store(&self) -> &Arc<JobStore> {
        &self.store
    }

    pub fn job(&self) -> Result<Job, String> {
        self.store
            .get(&self.id)?
            .ok_or_else(|| "job not found".to_string())
    }

    /// The job's `input_json` (Null if the row is gone).
    pub fn input(&self) -> Value {
        self.job().map(|job| job.input_json).unwrap_or(Value::Null)
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// Shared flag for code that polls cancellation (e.g. the worker host).
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.cancel)
    }

    /// Persists progress and emits `job://update`. `total` 0 = indeterminate.
    pub fn progress(&self, done: i64, total: i64, stage: &str) {
        let total = total.max(0);
        let done = if total > 0 {
            done.clamp(0, total)
        } else {
            done.max(0)
        };
        let _ = self.store.update(
            &self.id,
            &format!(
                "UPDATE jobs SET progress_done = ?2, progress_total = ?3, stage = ?4,
                   updated_at = {NOW}
                 WHERE id = ?1 AND status = 'running'
                   AND (progress_done != ?2 OR progress_total != ?3 OR stage != ?4)"
            ),
            &[&self.id, &done, &total, &stage],
        );
    }

    pub fn succeed(&self, output: Value) -> Result<Job, String> {
        self.store
            .finish(&self.id, JobStatus::Succeeded, Some(&output), None)
    }

    pub fn fail(&self, code: &str, retryable: bool, message: &str) -> Result<Job, String> {
        self.store.finish(
            &self.id,
            JobStatus::Failed,
            None,
            Some((code, retryable, message)),
        )
    }

    pub fn cancelled(&self) -> Result<Job, String> {
        self.store.finish(
            &self.id,
            JobStatus::Cancelled,
            None,
            Some(("cancelled", false, "cancelled by user")),
        )
    }

    /// Runs one worker request (spawn → handshake on `op` → request → shutdown),
    /// forwarding `progress` into this job and honouring `cancel_job`.
    pub fn run_worker(
        &self,
        spec: &WorkerSpec,
        op: &str,
        input: Value,
        input_files: Vec<PathBuf>,
        out_dir: PathBuf,
    ) -> Result<WorkerOutput, JobError> {
        let request = WorkerRequest {
            id: self.id.clone(),
            op: op.to_string(),
            input,
            input_files,
            out_dir,
        };
        let cancel = self.cancel_flag();
        worker::run_worker(spec, &request, &cancel, &mut |event| {
            if let WorkerEvent::Progress { stage, done, total } = event {
                self.progress(
                    i64::try_from(done).unwrap_or(i64::MAX),
                    i64::try_from(total).unwrap_or(i64::MAX),
                    &stage,
                );
            }
        })
        .map_err(JobError::from)
    }
}

impl Drop for JobHandle {
    fn drop(&mut self) {
        let still_running = self
            .store
            .get(&self.id)
            .ok()
            .flatten()
            .is_some_and(|job| job.status == JobStatus::Running);
        if still_running {
            let _ = self.store.finish(
                &self.id,
                JobStatus::Failed,
                None,
                Some((
                    "job_abandoned",
                    true,
                    "the job runner stopped without reporting a result",
                )),
            );
        } else {
            self.store.forget_active(&self.id);
        }
    }
}

fn get_job(conn: &Connection, id: &str) -> rusqlite::Result<Option<Job>> {
    conn.query_row("SELECT * FROM jobs WHERE id = ?1", [id], row_to_job)
        .optional()
}

fn row_to_job(row: &Row<'_>) -> rusqlite::Result<Job> {
    let status: String = row.get("status")?;
    let input: String = row.get("input_json")?;
    let output: Option<String> = row.get("output_json")?;
    Ok(Job {
        id: row.get("id")?,
        kind: row.get("kind")?,
        media_id: row.get("media_id")?,
        segment_id: row.get("segment_id")?,
        status: JobStatus::parse(&status).unwrap_or(JobStatus::Failed),
        progress_done: row.get("progress_done")?,
        progress_total: row.get("progress_total")?,
        stage: row.get("stage")?,
        error_code: row.get("error_code")?,
        error_message: row.get("error_message")?,
        retryable: row.get::<_, i64>("retryable")? != 0,
        attempt: row.get("attempt")?,
        input_json: serde_json::from_str(&input).unwrap_or(Value::Null),
        output_json: output.and_then(|text| serde_json::from_str(&text).ok()),
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

// ----- Tauri surface -----

/// Managed Tauri state.
pub struct JobsState(pub Arc<JobStore>);

/// Opens the job store on the product database, wires `job://update` and
/// registers it as Tauri state. Returns the store so other stages can register
/// runners.
pub fn install(
    app: &mut tauri::App,
    db_path: &Path,
) -> Result<Arc<JobStore>, Box<dyn std::error::Error>> {
    use tauri::{Emitter, Manager};
    let store = JobStore::open(db_path)?;
    let handle = app.handle().clone();
    store.set_listener(Arc::new(move |job: &Job| {
        let _ = handle.emit(JOB_UPDATE_EVENT, job);
    }));
    app.manage(JobsState(Arc::clone(&store)));
    Ok(store)
}

#[tauri::command]
pub fn list_jobs(
    filter: Option<JobFilter>,
    state: tauri::State<'_, JobsState>,
) -> Result<Vec<Job>, String> {
    state.0.list(&filter.unwrap_or_default())
}

#[tauri::command]
pub fn cancel_job(id: String, state: tauri::State<'_, JobsState>) -> Result<Job, String> {
    state.0.cancel(&id)
}

#[tauri::command]
pub fn retry_job(id: String, state: tauri::State<'_, JobsState>) -> Result<Job, String> {
    state.0.retry(&id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::worker::test_support::{echo_spec, temp_dir};
    use serde_json::json;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn store() -> Arc<JobStore> {
        JobStore::open_in_memory().unwrap()
    }

    fn wait_terminal(store: &JobStore, id: &str) -> Job {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let job = store.get(id).unwrap().unwrap();
            if job.status.is_terminal() {
                return job;
            }
            assert!(Instant::now() < deadline, "job did not finish: {job:?}");
            thread::sleep(Duration::from_millis(10));
        }
    }

    #[test]
    fn migrate_is_idempotent_and_matches_contract_columns() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();
        let mut stmt = conn.prepare("PRAGMA table_info(jobs)").unwrap();
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            columns,
            [
                "id",
                "kind",
                "media_id",
                "segment_id",
                "status",
                "progress_done",
                "progress_total",
                "stage",
                "error_code",
                "error_message",
                "retryable",
                "attempt",
                "input_json",
                "output_json",
                "created_at",
                "updated_at"
            ]
        );
    }

    #[test]
    fn media_db_open_creates_jobs_table() {
        let db = crate::db::MediaDb::open(Path::new(":memory:")).unwrap();
        drop(db);
        // MediaDb's hook runs migrate(); a file DB proves both connections agree.
        let path = std::env::temp_dir().join(format!("tsubame-jobs-{}.db", uuid::Uuid::new_v4()));
        drop(crate::db::MediaDb::open(&path).unwrap());
        let conn = Connection::open(&path).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE type='table' AND name='jobs'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn happy_path_transitions_and_events() {
        let store = store();
        let (tx, rx) = mpsc::channel();
        store.set_listener(Arc::new(move |job: &Job| {
            let _ = tx.send((job.status, job.progress_done, job.stage.clone()));
        }));
        let job = store
            .create(
                NewJob::new("test.kind", 7)
                    .segment(3)
                    .input(json!({"a": 1})),
            )
            .unwrap();
        assert_eq!((job.status, job.attempt), (JobStatus::Queued, 1));
        assert_eq!(job.segment_id, Some(3));
        assert_eq!(job.input_json, json!({"a": 1}));

        let handle = store.start(&job.id).unwrap();
        handle.progress(1, 2, "half");
        handle.progress(1, 2, "half"); // unchanged → no event
        let done = handle.succeed(json!({"ok": true})).unwrap();
        assert_eq!(done.status, JobStatus::Succeeded);
        assert_eq!(done.progress_done, 2);
        assert_eq!(done.output_json, Some(json!({"ok": true})));
        drop(handle);
        assert_eq!(
            store.get(&job.id).unwrap().unwrap().status,
            JobStatus::Succeeded
        );

        let events = rx.try_iter().collect::<Vec<_>>();
        assert_eq!(
            events,
            vec![
                (JobStatus::Queued, 0, String::new()),
                (JobStatus::Running, 0, String::new()),
                (JobStatus::Running, 1, "half".into()),
                (JobStatus::Succeeded, 2, "half".into()),
            ]
        );
    }

    #[test]
    fn illegal_transitions_are_refused() {
        let store = store();
        let job = store.create(NewJob::new("k", 1)).unwrap();
        let handle = store.start(&job.id).unwrap();
        assert!(store.start(&job.id).is_err(), "cannot start twice");
        handle.fail("x", false, "nope").unwrap();
        assert!(handle.succeed(json!({})).is_err(), "terminal is final");
        assert!(store.cancel(&job.id).is_err());
        assert!(store.create(NewJob::new("  ", 1)).is_err());
    }

    #[test]
    fn retry_rules() {
        let store = store();
        store.register_runner(
            "k",
            Arc::new(|_job: &JobHandle| Ok(json!({"second": true}))),
        );

        // Non-retryable failure cannot be retried.
        let hard = store.create(NewJob::new("k", 1)).unwrap();
        store
            .start(&hard.id)
            .unwrap()
            .fail("bad_input", false, "x")
            .unwrap();
        assert!(store.retry(&hard.id).unwrap_err().contains("non-retryable"));

        // Succeeded and cancelled jobs cannot be retried.
        let ok = store.create(NewJob::new("k", 1)).unwrap();
        store.start(&ok.id).unwrap().succeed(json!({})).unwrap();
        assert!(store.retry(&ok.id).is_err());
        let cancelled = store.create(NewJob::new("k", 1)).unwrap();
        store.cancel(&cancelled.id).unwrap();
        assert!(store.retry(&cancelled.id).is_err());

        // Retryable failure → queued, attempt + 1, error cleared, then runs again.
        let soft = store.create(NewJob::new("k", 1)).unwrap();
        let handle = store.start(&soft.id).unwrap();
        handle.progress(1, 3, "a");
        handle.fail("rate_limited", true, "429").unwrap();
        drop(handle);
        let queued = store.retry(&soft.id).unwrap();
        assert_eq!(queued.status, JobStatus::Queued);
        assert_eq!(queued.attempt, 2);
        assert_eq!((queued.error_code, queued.progress_done), (None, 0));
        let finished = wait_terminal(&store, &soft.id);
        assert_eq!(finished.status, JobStatus::Succeeded);
        assert_eq!(finished.attempt, 2);
        assert_eq!(finished.output_json, Some(json!({"second": true})));

        // Unknown kind cannot be retried (nothing would run it).
        let orphan = store.create(NewJob::new("no-runner", 1)).unwrap();
        store
            .start(&orphan.id)
            .unwrap()
            .fail("x", true, "y")
            .unwrap();
        assert!(store.retry(&orphan.id).unwrap_err().contains("no runner"));
        assert_eq!(store.get(&orphan.id).unwrap().unwrap().attempt, 1);
    }

    #[test]
    fn cancel_queued_and_running() {
        let store = store();
        let queued = store.create(NewJob::new("k", 1)).unwrap();
        let cancelled = store.cancel(&queued.id).unwrap();
        assert_eq!(cancelled.status, JobStatus::Cancelled);
        assert!(store.start(&queued.id).is_err());

        store.register_runner(
            "loop",
            Arc::new(|job: &JobHandle| {
                for i in 0..2000 {
                    if job.is_cancelled() {
                        return Err(JobError::cancelled());
                    }
                    job.progress(i, 2000, "loop");
                    thread::sleep(Duration::from_millis(5));
                }
                Ok(json!({}))
            }),
        );
        let job = store.submit(NewJob::new("loop", 1)).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while store.get(&job.id).unwrap().unwrap().progress_done == 0 {
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        let during = store.cancel(&job.id).unwrap();
        assert_eq!(
            during.status,
            JobStatus::Running,
            "flag set, runner still unwinding"
        );
        let finished = wait_terminal(&store, &job.id);
        assert_eq!(finished.status, JobStatus::Cancelled);
        assert_eq!(finished.error_code.as_deref(), Some("cancelled"));
        assert!(store.retry(&job.id).is_err());
    }

    #[test]
    fn runner_failure_panic_and_abandonment() {
        let store = store();
        store.register_runner(
            "fails",
            Arc::new(|_: &JobHandle| Err(JobError::new("boom", true, "x"))),
        );
        store.register_runner("panics", Arc::new(|_: &JobHandle| panic!("kaput")));
        let failed = store.submit(NewJob::new("fails", 1)).unwrap();
        let failed = wait_terminal(&store, &failed.id);
        assert_eq!(
            (
                failed.status,
                failed.error_code.as_deref(),
                failed.retryable
            ),
            (JobStatus::Failed, Some("boom"), true)
        );
        let panicked = store.submit(NewJob::new("panics", 1)).unwrap();
        let panicked = wait_terminal(&store, &panicked.id);
        assert_eq!(panicked.error_code.as_deref(), Some("panic"));
        assert_eq!(panicked.error_message.as_deref(), Some("kaput"));
        assert!(!panicked.retryable);

        let job = store.create(NewJob::new("k", 1)).unwrap();
        drop(store.start(&job.id).unwrap());
        let abandoned = store.get(&job.id).unwrap().unwrap();
        assert_eq!(abandoned.error_code.as_deref(), Some("job_abandoned"));
        assert!(abandoned.retryable);
        assert!(store.submit(NewJob::new("unregistered", 1)).is_err());
    }

    #[test]
    fn restart_marks_unfinished_jobs_interrupted() {
        let path = std::env::temp_dir().join(format!("tsubame-jobs-{}.db", uuid::Uuid::new_v4()));
        let (queued, running, done) = {
            let store = JobStore::open(&path).unwrap();
            let queued = store.create(NewJob::new("k", 1)).unwrap();
            let running = store.create(NewJob::new("k", 1)).unwrap();
            let handle = store.start(&running.id).unwrap();
            std::mem::forget(handle); // simulate a crash: no Drop, row stays running
            let done = store.create(NewJob::new("k", 1)).unwrap();
            store.start(&done.id).unwrap().succeed(json!({})).unwrap();
            (queued.id, running.id, done.id)
        };
        let store = JobStore::open(&path).unwrap();
        for id in [&queued, &running] {
            let job = store.get(id).unwrap().unwrap();
            assert_eq!(job.status, JobStatus::Failed);
            assert_eq!(job.error_code.as_deref(), Some("interrupted"));
            assert!(job.retryable);
        }
        assert_eq!(
            store.get(&done).unwrap().unwrap().status,
            JobStatus::Succeeded
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn orphaned_running_row_cancels_immediately() {
        let store = store();
        let job = store.create(NewJob::new("k", 1)).unwrap();
        std::mem::forget(store.start(&job.id).unwrap());
        store.forget_active(&job.id);
        assert_eq!(store.cancel(&job.id).unwrap().status, JobStatus::Cancelled);
    }

    #[test]
    fn list_filters_and_orders_newest_first() {
        let store = store();
        let a = store.create(NewJob::new("text.translate", 1)).unwrap();
        let b = store
            .create(NewJob::new("speech.tts", 1).segment(9))
            .unwrap();
        let c = store.create(NewJob::new("speech.tts", 2)).unwrap();
        store.cancel(&c.id).unwrap();
        let ids = |filter: JobFilter| {
            store
                .list(&filter)
                .unwrap()
                .into_iter()
                .map(|job| job.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(JobFilter::default()),
            vec![c.id.clone(), b.id.clone(), a.id.clone()]
        );
        assert_eq!(
            ids(JobFilter {
                media_id: Some(1),
                ..Default::default()
            }),
            vec![b.id.clone(), a.id.clone()]
        );
        assert_eq!(
            ids(JobFilter {
                segment_id: Some(9),
                ..Default::default()
            }),
            vec![b.id.clone()]
        );
        assert_eq!(
            ids(JobFilter {
                kind: Some("speech.tts".into()),
                status: Some("queued".into()),
                ..Default::default()
            }),
            vec![b.id.clone()]
        );
        assert_eq!(
            ids(JobFilter {
                limit: Some(1),
                ..Default::default()
            }),
            vec![c.id]
        );
        assert!(store
            .list(&JobFilter {
                status: Some("bogus".into()),
                ..Default::default()
            })
            .is_err());
    }

    #[test]
    fn worker_backed_job_streams_progress_and_cancels() {
        let Some(spec) = echo_spec(&[]) else {
            eprintln!("python3 not available; skipping");
            return;
        };
        let store = store();
        let spec_ok = spec.clone();
        store.register_runner(
            "test.echo",
            Arc::new(move |job: &JobHandle| {
                let input = job.input();
                let out = job.run_worker(
                    &spec_ok,
                    "test.echo",
                    input,
                    Vec::new(),
                    temp_dir("job-out"),
                )?;
                Ok(out.output)
            }),
        );
        let (tx, rx) = mpsc::channel();
        store.set_listener(Arc::new(move |job: &Job| {
            let _ = tx.send(job.clone());
        }));

        let job = store
            .submit(NewJob::new("test.echo", 1).input(json!({"mode": "progress", "steps": 3})))
            .unwrap();
        let finished = wait_terminal(&store, &job.id);
        assert_eq!(finished.status, JobStatus::Succeeded);
        assert_eq!(finished.output_json, Some(json!({"steps": 3})));
        let progress = rx
            .try_iter()
            .filter(|row| row.status == JobStatus::Running && row.stage == "echo")
            .map(|row| (row.progress_done, row.progress_total))
            .collect::<Vec<_>>();
        assert_eq!(progress, vec![(1, 3), (2, 3), (3, 3)]);

        let failing = store
            .submit(
                NewJob::new("test.echo", 1)
                    .input(json!({"mode": "error", "code": "oom", "retryable": false})),
            )
            .unwrap();
        let failing = wait_terminal(&store, &failing.id);
        assert_eq!(
            (failing.status, failing.error_code.as_deref()),
            (JobStatus::Failed, Some("oom"))
        );

        let slow = store
            .submit(NewJob::new("test.echo", 1).input(json!({"mode": "slow", "steps": 1000})))
            .unwrap();
        let deadline = Instant::now() + Duration::from_secs(15);
        while store.get(&slow.id).unwrap().unwrap().stage != "slow" {
            assert!(Instant::now() < deadline, "worker never reported progress");
            thread::sleep(Duration::from_millis(10));
        }
        store.cancel(&slow.id).unwrap();
        let slow = wait_terminal(&store, &slow.id);
        assert_eq!(slow.status, JobStatus::Cancelled);
    }
}
