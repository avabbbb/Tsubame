//! CLI-only agent end-to-end test: the docs/01-product.md flow driven through
//! yuzuki --json (yuzuki_cli::run, in-process) against the real control
//! endpoint over a file-backed fixture Desktop, then checked by reading the
//! SQLite file directly. Step table and gaps: docs/07-agent-control.md
//! ("Agent end-to-end test").
//!
//! No GUI, no direct operation calls and no process spawning here (see
//! tests/golden.rs for why spawning stays out of in-process endpoint tests).
//! Steps with no registry operation are the explicit #[ignore = "gap: ..."]
//! tests at the end of this file; never fake them in the CLI.

use rusqlite::{Connection, OpenFlags, OptionalExtension};
use serde_json::Value;
use tsubame_lib::test_support::{start_file_backed_desktop, FileFixtureDesktop};

/// Runs one CLI command and returns its data; any failure fails the test.
fn yuzuki(d: &FileFixtureDesktop, args: &[&str]) -> Value {
    let mut argv: Vec<String> = vec![
        "yuzuki".into(),
        "--json".into(),
        "--control-file".into(),
        d.control_file.display().to_string(),
    ];
    argv.extend(args.iter().map(|arg| arg.to_string()));
    let out = yuzuki_cli::run(argv);
    assert_eq!(
        out.code, 0,
        "{args:?}: stdout {} stderr {}",
        out.stdout, out.stderr
    );
    assert!(out.stderr.is_empty(), "{args:?}: stderr {}", out.stderr);
    let envelope: Value = serde_json::from_str(&out.stdout)
        .unwrap_or_else(|e| panic!("{args:?}: stdout is not JSON ({e}): {}", out.stdout));
    assert_eq!(envelope["ok"], true, "{args:?}: {envelope}");
    envelope["data"].clone()
}

fn int(value: &Value) -> i64 {
    value
        .as_i64()
        .unwrap_or_else(|| panic!("not an integer: {value}"))
}

struct SegmentRow {
    translated_text: String,
    revision: i64,
    dirty_translation: bool,
    dirty_tts: bool,
    tts_asset_key: Option<String>,
}

fn segment_row(conn: &Connection, id: i64) -> SegmentRow {
    conn.query_row(
        "SELECT translated_text, revision, dirty_translation, dirty_tts, tts_asset_key
           FROM segments WHERE id = ?1",
        [id],
        |row| {
            Ok(SegmentRow {
                translated_text: row.get(0)?,
                revision: row.get(1)?,
                dirty_translation: row.get(2)?,
                dirty_tts: row.get(3)?,
                tts_asset_key: row.get(4)?,
            })
        },
    )
    .unwrap()
}

#[test]
fn reachable_steps_in_order() {
    let d = start_file_backed_desktop();
    let media = d.media_id.to_string();

    // Step 1 (install): only "the Desktop is up and answers" is checkable.
    assert_eq!(yuzuki(&d, &["app", "status"])["running"], true);

    // Step 3 (choose an ASR provider): provider.list, reachable only through call.
    // ProviderConfig carries no capabilities, so check that the pack-managed
    // local ASR provider (runtime.plan/runtime.install target) is offered.
    let providers = yuzuki(&d, &["call", "provider.list"]);
    let ids: Vec<&str> = providers
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&"local.faster-whisper"),
        "no local ASR provider in {ids:?}"
    );

    // Step 5 (edit one sentence): translate-edit the first Segment by hand.
    let before = yuzuki(&d, &["segment", "list", "--media-id", &media]);
    let before = before.as_array().unwrap();
    assert_eq!(before.len(), 2);
    let (a, b) = (d.segment_ids[0], d.segment_ids[1]);
    assert_eq!(int(&before[0]["id"]), a);
    let b_revision = int(&before[1]["revision"]);
    let edited = yuzuki(
        &d,
        &[
            "segment",
            "update",
            "--id",
            &a.to_string(),
            "--expected-revision",
            &before[0]["revision"].to_string(),
            "--translated-text",
            "Good night.",
        ],
    );
    assert_eq!(edited["translated_text"], "Good night.");
    assert_eq!(int(&edited["revision"]), int(&before[0]["revision"]) + 1);
    assert_eq!(edited["dirty_translation"], false);
    assert_eq!(edited["dirty_tts"], true);

    // Step 9 (regenerate only that sentence): blocks until the job finishes.
    yuzuki(
        &d,
        &[
            "segment",
            "regenerate",
            "--segment-id",
            &a.to_string(),
            "--expected-revision",
            &edited["revision"].to_string(),
            "--voice-profile-id",
            &d.voice_profile_id,
        ],
    );
    let jobs = yuzuki(&d, &["job", "list", "--media-id", &media]);
    let jobs = jobs.as_array().unwrap();
    assert_eq!(jobs.len(), 1, "{jobs:?}");
    assert_eq!(
        (jobs[0]["kind"].as_str(), jobs[0]["status"].as_str()),
        (Some("regenerate"), Some("succeeded"))
    );
    assert_eq!(int(&jobs[0]["segment_id"]), a);

    // The CLI's view after the run.
    let after = yuzuki(&d, &["segment", "list", "--media-id", &media]);
    let after = after.as_array().unwrap();

    // Final state, read from the SQLite file itself.
    let conn = Connection::open_with_flags(&d.db_path, OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let row_a = segment_row(&conn, a);
    assert_eq!(row_a.translated_text, "Good night.");
    assert_eq!(row_a.revision, int(&after[0]["revision"]));
    assert!(row_a.revision >= int(&edited["revision"]));
    assert!(!row_a.dirty_translation);
    assert!(!row_a.dirty_tts, "regenerate leaves the sentence dirty");
    let key = row_a
        .tts_asset_key
        .expect("regenerated sentence has an active take");
    let asset: Option<String> = conn
        .query_row("SELECT key FROM tts_assets WHERE key = ?1", [&key], |row| {
            row.get(0)
        })
        .optional()
        .unwrap();
    assert_eq!(asset.as_deref(), Some(key.as_str()));

    // Only that sentence: the other Segment is untouched.
    let row_b = segment_row(&conn, b);
    assert_eq!(
        (row_b.translated_text.as_str(), row_b.revision),
        ("", b_revision)
    );
    assert_eq!(row_b.tts_asset_key, None);

    let job_rows: Vec<(String, Option<i64>, String)> = conn
        .prepare("SELECT kind, segment_id, status FROM jobs ORDER BY created_at")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        job_rows,
        vec![("regenerate".to_string(), Some(a), "succeeded".to_string())]
    );
}

// ----- Gaps: steps with no Operation Registry op, so no CLI path. -----

#[test]
#[ignore = "gap: step 1 install has no registry op (installer; the CLI can only check app status)"]
fn step_01_install() {
    panic!("gap: install");
}

#[test]
fn step_02_import_local_media() {
    let d = start_file_backed_desktop();
    let dir = std::env::temp_dir().join(format!("yuzuki-e2e-import-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("nested")).unwrap();
    std::fs::write(dir.join("nested").join("voice.wav"), b"RIFF").unwrap();
    std::fs::write(
        dir.join("nested").join("voice.srt"),
        "1\n00:00:00,500 --> 00:00:02,000\nおはようございます\n\n",
    )
    .unwrap();

    let imported = yuzuki(&d, &["media", "import-folder", dir.to_str().unwrap()]);
    let imported = imported.as_array().unwrap();
    assert_eq!(imported.len(), 1, "{imported:?}");
    let media_id = int(&imported[0]["id"]);
    assert_ne!(media_id, d.media_id);

    let listed = yuzuki(&d, &["media", "list"]);
    assert!(listed
        .as_array()
        .unwrap()
        .iter()
        .any(|item| int(&item["id"]) == media_id));

    let segments = yuzuki(
        &d,
        &["segment", "list", "--media-id", &media_id.to_string()],
    );
    assert!(
        segments.to_string().contains("おはようございます"),
        "sidecar did not seed segments: {segments}"
    );

    // Importing the same folder again is idempotent for the library.
    let again = yuzuki(&d, &["media", "import-folder", dir.to_str().unwrap()]);
    assert_eq!(int(&again[0]["id"]), media_id);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "gap: step 3 installing an ASR provider is not run (runtime.install downloads packs; no install fixture)"]
fn step_03_install_asr_runtime() {
    panic!("gap: runtime.install against a fixture pack source");
}

#[test]
#[ignore = "gap: step 4 asr.run is a registry op, but the fixture Desktop has no ASR runtime or runner; transcription is verified against a real Desktop (docs/07-agent-control.md)"]
fn step_04_generate_sentence_subtitles() {
    panic!("gap: transcribe");
}

#[test]
#[ignore = "gap: step 6 BYOK translation provider has no registry op (no provider.save / secret set)"]
fn step_06_configure_translation_provider() {
    panic!("gap: provider configuration");
}

#[test]
#[ignore = "gap: step 7 translate has no registry op (no segment.translate)"]
fn step_07_translate() {
    panic!("gap: translate");
}

#[test]
#[ignore = "gap: step 8 TTS provider/voice profile has no registry op (no provider.save / voice.save)"]
fn step_08_configure_tts() {
    panic!("gap: TTS configuration");
}

#[test]
#[ignore = "gap: step 10 playback has no registry op (no player / playback state op)"]
fn step_10_continue_playback_with_generated_variant() {
    panic!("gap: playback");
}
