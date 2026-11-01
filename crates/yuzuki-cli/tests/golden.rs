//! Golden JSON for yuzuki --json against a real control endpoint over an
//! in-memory Desktop. Regenerate with YUZUKI_BLESS=1 and review the diff.
//!
//! Nothing here spawns processes: on Windows a child started while the
//! in-process endpoint accepts can inherit the accepted socket and reset it
//! (os error 10054). Binary-level checks live in tests/exit_codes.rs, which
//! runs as its own process.

use serde_json::Value;
use std::path::PathBuf;
use tsubame_lib::test_support::{start_fixture_desktop, FixtureDesktop};
use yuzuki_cli::Outcome;

fn yuzuki(control_file: &str, args: &[&str]) -> Outcome {
    let mut argv: Vec<String> = vec![
        "yuzuki".into(),
        "--json".into(),
        "--control-file".into(),
        control_file.into(),
    ];
    argv.extend(args.iter().map(|arg| arg.to_string()));
    yuzuki_cli::run(argv)
}

fn on(d: &FixtureDesktop, args: &[&str]) -> Outcome {
    yuzuki(&d.control_file.display().to_string(), args)
}

fn envelope(out: &Outcome) -> Value {
    assert!(out.stderr.is_empty(), "stderr: {}", out.stderr);
    serde_json::from_str(&out.stdout)
        .unwrap_or_else(|e| panic!("stdout is not JSON ({e}): {}", out.stdout))
}

/// Timestamps differ per run; everything else must match byte for byte.
fn normalize(value: &mut Value) {
    match value {
        Value::Object(map) => {
            for (key, field) in map.iter_mut() {
                if key.ends_with("_at") && !field.is_null() {
                    *field = Value::String("<timestamp>".into());
                } else {
                    normalize(field);
                }
            }
        }
        Value::Array(items) => items.iter_mut().for_each(normalize),
        _ => {}
    }
}

fn golden(name: &str, out: &Outcome, exit: u8) {
    assert_eq!(
        out.code, exit,
        "{name}: stdout {} stderr {}",
        out.stdout, out.stderr
    );
    let mut value = envelope(out);
    normalize(&mut value);
    let actual = serde_json::to_string_pretty(&value).unwrap() + "\n";
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(format!("{name}.json"));
    if std::env::var_os("YUZUKI_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}; run with YUZUKI_BLESS=1", path.display()));
    assert_eq!(
        actual,
        expected.replace("\r\n", "\n"),
        "golden {name} changed"
    );
}

fn error_code(out: &Outcome) -> String {
    let value = envelope(out);
    assert_eq!(value["ok"], false, "{value}");
    value["error"]["code"].as_str().unwrap().to_string()
}

#[test]
fn segment_list() {
    let d = start_fixture_desktop();
    golden(
        "segment_list",
        &on(
            &d,
            &["segment", "list", "--media-id", &d.media_id.to_string()],
        ),
        0,
    );
}

#[test]
fn segment_update_then_stale_revision() {
    let d = start_fixture_desktop();
    let args = [
        "segment".to_string(),
        "update".into(),
        "--id".into(),
        d.segment_id.to_string(),
        "--expected-revision".into(),
        d.revision.to_string(),
        "--translated-text".into(),
        "Good night.".into(),
    ];
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    golden("segment_update", &on(&d, &args), 0);
    // The same revision again is now stale: refused, nothing changes.
    golden("segment_update_stale", &on(&d, &args), 5);
    golden(
        "segment_list_after_update",
        &on(
            &d,
            &["segment", "list", "--media-id", &d.media_id.to_string()],
        ),
        0,
    );
}

#[test]
fn job_list_when_idle() {
    let d = start_fixture_desktop();
    golden("job_list", &on(&d, &["job", "list"]), 0);
}

#[test]
fn ops_list_names_every_registered_operation() {
    let d = start_fixture_desktop();
    let out = on(&d, &["ops", "list"]);
    assert_eq!(out.code, 0, "{}", out.stdout);
    let value = envelope(&out);
    let names: Vec<&str> = value["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|op| op["name"].as_str().unwrap())
        .collect();
    for name in [
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
        "asr.run",
        "translate.run",
        "tts.synthesize",
        "spatial.analyze",
        "spatial.render",
        "mix.export",
    ] {
        assert!(names.contains(&name), "{name} missing from {names:?}");
    }
    assert!(value["data"][0]["input_schema"].is_object(), "{value}");
}

#[test]
fn app_status_reports_the_running_desktop() {
    let d = start_fixture_desktop();
    let out = on(&d, &["app", "status"]);
    assert_eq!(out.code, 0, "{}", out.stdout);
    assert_eq!(envelope(&out)["data"]["running"], true);
}

#[test]
fn unknown_input_fields_are_invalid_input() {
    let d = start_fixture_desktop();
    let out = on(&d, &["call", "segment.list", "--input", r#"{"bogus":1}"#]);
    assert_eq!((out.code, error_code(&out).as_str()), (7, "invalid_input"));
}

#[test]
fn a_missing_desktop_exits_3() {
    let missing = std::env::temp_dir()
        .join("yuzuki-cli-no-such-dir")
        .join("control.json");
    let out = yuzuki(
        &missing.display().to_string(),
        &["segment", "list", "--media-id", "1"],
    );
    assert_eq!(
        (out.code, error_code(&out).as_str()),
        (3, "desktop_not_running")
    );
}

#[test]
fn usage_errors_exit_2_with_an_envelope() {
    let out = yuzuki("unused", &["segment", "list"]);
    assert_eq!((out.code, error_code(&out).as_str()), (2, "usage"));
}

#[test]
fn app_ensure_on_a_running_desktop_does_not_launch() {
    let d = start_fixture_desktop();
    let out = on(&d, &["app", "ensure"]);
    assert_eq!(out.code, 0, "{}", out.stdout);
    let data = &envelope(&out)["data"];
    assert_eq!(data["running"], true, "{data}");
    assert_eq!(data["launched"], false, "{data}");
    assert_eq!(data["control_file"], d.control_file.display().to_string());
}

#[test]
fn init_reports_the_running_desktop_and_doctor() {
    let d = start_fixture_desktop();
    let out = on(&d, &["init"]);
    assert_eq!(out.code, 0, "{}", out.stdout);
    let data = &envelope(&out)["data"];
    assert_eq!(data["desktop"]["running"], true, "{data}");
    assert!(
        data["desktop"]["operations"].as_u64().unwrap() > 0,
        "{data}"
    );
    assert!(
        data["doctor"]
            .as_array()
            .is_some_and(|packs| !packs.is_empty()),
        "{data}"
    );
    assert_eq!(data["cli"]["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(data["cli"]["os"], std::env::consts::OS);
}

#[test]
fn init_without_a_desktop_is_a_report_not_an_error() {
    let missing = std::env::temp_dir()
        .join("yuzuki-cli-no-such-dir")
        .join("control.json");
    let out = yuzuki(&missing.display().to_string(), &["init"]);
    assert_eq!(out.code, 0, "{}", out.stdout);
    let data = &envelope(&out)["data"];
    assert_eq!(data["desktop"]["running"], false, "{data}");
    assert!(data["doctor"].is_null(), "{data}");
    assert!(data["next_steps"][0]
        .as_str()
        .unwrap()
        .contains("app ensure"));
}
