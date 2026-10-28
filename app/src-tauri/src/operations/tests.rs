use super::*;
use crate::subtitle::SubtitleCue;
use serde_json::json;
use std::path::Path;

struct Fixture {
    db: Mutex<MediaDb>,
    jobs: Arc<JobStore>,
    media_id: i64,
    segment: Segment,
}

impl Fixture {
    fn new() -> Self {
        let mut db = MediaDb::open(Path::new(":memory:")).unwrap();
        let media = db.import_media("C:/fixtures/operations.wav").unwrap();
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
        Self {
            db: Mutex::new(db),
            jobs: JobStore::open_in_memory().unwrap(),
            media_id: media.id,
            segment,
        }
    }

    fn ctx(&self) -> OperationContext<'_> {
        OperationContext {
            db: &self.db,
            jobs: &self.jobs,
        }
    }
}

/// The path Tauri commands take: typed input through [invoke].
fn typed<O: Operation>(ctx: &OperationContext<'_>, input: &Value) -> Result<Value, OperationError> {
    let input: O::Input =
        serde_json::from_value(input.clone()).expect("input matches the typed Input");
    invoke::<O>(ctx, input).map(|output| serde_json::to_value(output).unwrap())
}

/// The path the control endpoint, CLI and MCP take: JSON by name.
fn by_name<O: Operation>(
    ctx: &OperationContext<'_>,
    input: &Value,
) -> Result<Value, OperationError> {
    invoke_json(ctx, O::META.name, input.clone())
}

/// Read-only operations: both paths against one fixture.
fn same_read<O: Operation>(fx: &Fixture, input: Value) -> Result<Value, OperationError> {
    let direct = typed::<O>(&fx.ctx(), &input);
    let json = by_name::<O>(&fx.ctx(), &input);
    assert_eq!(
        direct,
        json,
        "{} differs between typed and JSON dispatch",
        O::META.name
    );
    json
}

/// Mutating operations: each path against its own identical fixture, so the
/// first call cannot change what the second one sees.
fn same_write<O: Operation>(input: impl Fn(&Fixture) -> Value) -> Result<Value, OperationError> {
    let (a, b) = (Fixture::new(), Fixture::new());
    let direct = typed::<O>(&a.ctx(), &input(&a)).map(strip_volatile);
    let json = by_name::<O>(&b.ctx(), &input(&b)).map(strip_volatile);
    assert_eq!(
        direct,
        json,
        "{} differs between typed and JSON dispatch",
        O::META.name
    );
    json
}

/// Drops generated ids and timestamps, which differ between two fixtures.
fn strip_volatile(mut value: Value) -> Value {
    match &mut value {
        Value::Object(map) => {
            map.retain(|key, _| key != "id" && !key.ends_with("_at"));
            for item in map.values_mut() {
                *item = strip_volatile(item.take());
            }
        }
        Value::Array(items) => {
            for item in items {
                *item = strip_volatile(item.take());
            }
        }
        _ => {}
    }
    value
}

fn code(result: &Result<Value, OperationError>) -> &str {
    &result.as_ref().unwrap_err().code
}

fn message(result: &Result<Value, OperationError>) -> &str {
    &result.as_ref().unwrap_err().message
}

#[test]
fn registry_names_are_unique_and_metadata_matches_the_contract() {
    let names: Vec<&str> = registry().iter().map(|entry| entry.meta.name).collect();
    assert_eq!(
        names,
        [
            "segment.list",
            "segment.update",
            "segment.regenerate",
            "job.list",
            "job.cancel",
            "runtime.doctor",
            "provider.list",
            "runtime.plan",
            "runtime.install",
            "media.list",
            "media.import_folder",
            "asr.run"
        ]
    );
    let meta = |name: &str| {
        registry()
            .iter()
            .find(|entry| entry.meta.name == name)
            .unwrap()
            .meta
    };
    for name in [
        "segment.list",
        "job.list",
        "runtime.doctor",
        "provider.list",
        "runtime.plan",
        "media.list",
    ] {
        assert!(!meta(name).mutates, "{name} must be read-only");
        assert!(
            meta(name).side_effects.is_empty(),
            "{name} declares no side effects"
        );
    }
    for name in [
        "segment.update",
        "segment.regenerate",
        "job.cancel",
        "runtime.install",
        "media.import_folder",
        "asr.run",
    ] {
        assert!(meta(name).mutates, "{name} mutates");
    }
    assert!(meta("segment.update").expected_revision);
    assert!(meta("segment.regenerate").expected_revision);
    assert!(!meta("job.cancel").expected_revision);
    assert!(!meta("runtime.install").expected_revision);
    assert_eq!(meta("segment.regenerate").cancellation, Cancellation::Job);
    assert_eq!(meta("runtime.install").cancellation, Cancellation::Job);
    assert_eq!(meta("asr.run").cancellation, Cancellation::Job);
    assert!(!meta("asr.run").expected_revision);
    assert_eq!(meta("media.import_folder").cancellation, Cancellation::None);
}

#[test]
fn segment_list_agrees_across_paths() {
    let fx = Fixture::new();
    let listed = same_read::<SegmentList>(&fx, json!({ "media_id": fx.media_id })).unwrap();
    assert_eq!(
        listed,
        serde_json::to_value(vec![fx.segment.clone()]).unwrap()
    );
}

#[test]
fn segment_update_agrees_across_paths() {
    let updated = same_write::<SegmentUpdate>(|fx| {
        json!({
            "id": fx.segment.id,
            "expected_revision": fx.segment.revision,
            "source_text": fx.segment.source_text,
            "translated_text": "Good night.",
            "start_ms": 1_000,
            "end_ms": 2_000
        })
    })
    .unwrap();
    assert_eq!(updated["translated_text"], "Good night.");
}

#[test]
fn segment_update_rejects_a_stale_revision_and_bad_timing_identically() {
    let stale = same_write::<SegmentUpdate>(|fx| {
        json!({
            "id": fx.segment.id,
            "expected_revision": fx.segment.revision + 1,
            "source_text": "x",
            "translated_text": "y",
            "start_ms": 1_000,
            "end_ms": 2_000
        })
    });
    assert_eq!(code(&stale), "revision_conflict");
    // The Inspector matches this exact text for its conflict UI.
    assert_eq!(message(&stale), "segment revision conflict");

    let backwards = same_write::<SegmentUpdate>(|fx| {
        json!({
            "id": fx.segment.id,
            "expected_revision": fx.segment.revision,
            "source_text": "x",
            "translated_text": "y",
            "start_ms": 2_000,
            "end_ms": 2_000
        })
    });
    assert_eq!(code(&backwards), "invalid_input");
    assert_eq!(message(&backwards), "segment end must be after start");

    // A rejected update changes nothing.
    let fx = Fixture::new();
    let _ = by_name::<SegmentUpdate>(
        &fx.ctx(),
        &json!({
            "id": fx.segment.id,
            "expected_revision": fx.segment.revision + 1,
            "source_text": "x",
            "translated_text": "y",
            "start_ms": 1_000,
            "end_ms": 2_000
        }),
    );
    assert_eq!(
        fx.db.lock().unwrap().get_segment(fx.segment.id).unwrap(),
        Some(fx.segment.clone())
    );
}

#[test]
fn segment_regenerate_errors_agree_across_paths() {
    let missing = same_write::<SegmentRegenerate>(
        |_| json!({ "segment_id": 9_999, "expected_revision": 0, "voice_profile_id": "voice-1" }),
    );
    assert_eq!(code(&missing), "not_found");
    // Same text the command returned before the registry.
    assert_eq!(message(&missing), "not_found: segment not found");

    let no_runner = same_write::<SegmentRegenerate>(|fx| {
        json!({
            "segment_id": fx.segment.id,
            "expected_revision": fx.segment.revision,
            "voice_profile_id": "voice-1"
        })
    });
    assert_eq!(code(&no_runner), "runner_missing");
}

#[test]
fn job_list_agrees_across_paths() {
    let fx = Fixture::new();
    fx.jobs.create(NewJob::new("k", fx.media_id)).unwrap();
    let all = same_read::<JobList>(&fx, json!({})).unwrap();
    assert_eq!(all.as_array().unwrap().len(), 1);
    let filtered = same_read::<JobList>(&fx, json!({ "filter": { "kind": "other" } })).unwrap();
    assert_eq!(filtered, json!([]));
    let bad = same_read::<JobList>(&fx, json!({ "filter": { "status": "bogus" } }));
    assert_eq!(message(&bad), "unknown job status: bogus");
}

#[test]
fn job_cancel_agrees_across_paths() {
    let cancelled = same_write::<JobCancel>(|fx| {
        let job = fx.jobs.create(NewJob::new("k", fx.media_id)).unwrap();
        json!({ "id": job.id })
    })
    .unwrap();
    assert_eq!(cancelled["status"], "cancelled");

    let unknown = same_write::<JobCancel>(|_| json!({ "id": "no-such-job" }));
    assert_eq!(code(&unknown), "job_store");
    assert_eq!(message(&unknown), "job not found");
}

#[test]
fn runtime_doctor_agrees_across_paths() {
    let fx = Fixture::new();
    let report = same_read::<RuntimeDoctor>(&fx, json!({})).unwrap();
    assert_eq!(
        by_name::<RuntimeDoctor>(&fx.ctx(), &Value::Null).unwrap(),
        report
    );
}

#[test]
fn provider_list_agrees_across_paths() {
    let fx = Fixture::new();
    let listed = same_read::<ProviderList>(&fx, json!({})).unwrap();
    let providers = listed.as_array().unwrap();
    assert!(
        !providers.is_empty(),
        "built-in local runtimes are always listed"
    );
    // System-managed providers come first, as in Settings.
    let managed: Vec<bool> = providers
        .iter()
        .map(|p| p["system_managed"].as_bool().unwrap())
        .collect();
    let mut sorted = managed.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(managed, sorted);
}

#[test]
fn json_dispatch_rejects_unknown_operations_and_malformed_input() {
    let fx = Fixture::new();
    let unknown = invoke_json(&fx.ctx(), "segment.delete", json!({}));
    assert_eq!(code(&unknown), "unknown_operation");
    let malformed = invoke_json(&fx.ctx(), "segment.list", json!({ "media": 1 }));
    assert_eq!(code(&malformed), "invalid_input");
}

#[test]
fn segment_update_keeps_omitted_fields() {
    let fx = Fixture::new();
    let input = json!({
        "id": fx.segment.id,
        "expected_revision": fx.segment.revision,
        "translated_text": "Good night.",
    });
    let updated = by_name::<SegmentUpdate>(&fx.ctx(), &input).unwrap();
    assert_eq!(updated["translated_text"], "Good night.");
    assert_eq!(updated["source_text"], fx.segment.source_text);
    assert_eq!(updated["start_ms"], fx.segment.start_ms);
    assert_eq!(updated["end_ms"], fx.segment.end_ms);
    // The same revision again is now stale.
    let stale = by_name::<SegmentUpdate>(&fx.ctx(), &input);
    assert_eq!(code(&stale), "revision_conflict");
}

#[test]
fn runtime_plan_rejects_a_provider_without_packs() {
    let fx = Fixture::new();
    let result = same_read::<RuntimePlan>(&fx, json!({ "provider_id": "remote.none" }));
    assert_eq!(code(&result), "no_packs");
    assert_eq!(message(&result), "remote.none has no installable packs");
}

fn temp_media_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("yuzuki-ops-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn media_list_agrees_across_paths() {
    let fx = Fixture::new();
    let listed = same_read::<MediaList>(&fx, json!({})).unwrap();
    let items = listed.as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["id"], json!(fx.media_id));
}

#[test]
fn media_import_folder_imports_media_and_seeds_sidecars() {
    let dir = temp_media_dir("import");
    std::fs::write(dir.join("voice.wav"), b"RIFF").unwrap();
    std::fs::write(
        dir.join("voice.srt"),
        "1\n00:00:01,000 --> 00:00:02,500\nこんにちは\n\n",
    )
    .unwrap();
    std::fs::write(dir.join("notes.txt"), "not media").unwrap();
    let path = dir.to_str().unwrap().to_string();

    let imported = same_write::<MediaImportFolder>(|_| json!({ "path": path })).unwrap();
    let items = imported.as_array().unwrap();
    assert_eq!(items.len(), 1, "only the wav is media: {imported}");
    assert!(items[0]["path"].as_str().unwrap().ends_with("voice.wav"));

    let fx = Fixture::new();
    let imported = by_name::<MediaImportFolder>(&fx.ctx(), &json!({ "path": path })).unwrap();
    let media_id = imported[0]["id"].as_i64().unwrap();
    let segments = invoke_json(&fx.ctx(), "segment.list", json!({ "media_id": media_id })).unwrap();
    assert!(
        segments.to_string().contains("こんにちは"),
        "sidecar seeded the transcript: {segments}"
    );
    let listed = by_name::<MediaList>(&fx.ctx(), &json!({})).unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn media_import_folder_rejects_relative_and_missing_paths() {
    let fx = Fixture::new();
    let relative = by_name::<MediaImportFolder>(&fx.ctx(), &json!({ "path": "test-media" }));
    assert_eq!(code(&relative), "invalid_input");
    let missing = temp_media_dir("missing").join("nope");
    let missing = by_name::<MediaImportFolder>(
        &fx.ctx(),
        &json!({ "path": missing.to_str().unwrap() }),
    );
    assert_eq!(code(&missing), "not_found");
}

#[test]
fn asr_run_queues_a_job_for_known_media_only() {
    let fx = Fixture::new();
    // The Desktop registers the real runner in setup; the operation only queues.
    fx.jobs.register_runner(
        crate::ASR_JOB,
        Arc::new(|_: &crate::jobs::JobHandle| Ok(json!({}))),
    );
    let unknown = by_name::<AsrRun>(
        &fx.ctx(),
        &json!({ "media_id": fx.media_id + 999, "provider_id": "local.faster-whisper" }),
    );
    assert_eq!(code(&unknown), "not_found");
    let job = by_name::<AsrRun>(
        &fx.ctx(),
        &json!({ "media_id": fx.media_id, "provider_id": "local.faster-whisper", "language": "ja" }),
    )
    .unwrap();
    assert_eq!(job["kind"], json!("asr"), "{job}");
    assert_eq!(job["media_id"], json!(fx.media_id), "{job}");
}
