//! Fixture Desktops for out-of-crate tests (feature test-support): the real
//! control endpoint and Operation Registry over an in-memory or a file-backed
//! database. Never compiled into release builds.

use crate::control::{self, Dispatch};
use crate::db::MediaDb;
use crate::jobs::{JobError, JobHandle, JobRunner, JobStore};
use crate::operations::{invoke_json, OperationContext};
use crate::regenerate::{run_regenerate, RegenerateInput, SpatialAvailability, REGENERATE_JOB};
use crate::subtitle::SubtitleCue;
use crate::tts::{AssetStore, MockTts};
use crate::voice::{self, VoiceProfileInput};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// A running endpoint; its directory is removed on drop.
pub struct FixtureDesktop {
    pub dir: PathBuf,
    pub control_file: PathBuf,
    pub media_id: i64,
    /// The one Segment the fixture holds ("晚安。", 1000–2000 ms).
    pub segment_id: i64,
    pub revision: i64,
}

impl Drop for FixtureDesktop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

pub fn start_fixture_desktop() -> FixtureDesktop {
    let mut db = MediaDb::open(Path::new(":memory:")).expect("in-memory db");
    let media = db
        .import_media("C:/fixtures/control.wav")
        .expect("fixture media");
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
        .expect("fixture segment")
        .remove(0);
    let state = Arc::new((
        Mutex::new(db),
        JobStore::open_in_memory().expect("in-memory jobs"),
    ));
    let dispatch: Dispatch = Arc::new(move |name, input| {
        let ctx = OperationContext {
            db: &state.0,
            jobs: &state.1,
        };
        invoke_json(&ctx, name, input)
    });
    let dir =
        std::env::temp_dir().join(format!("yuzuki-fixture-{}", uuid::Uuid::new_v4().simple()));
    control::start(&dir, dispatch).expect("control endpoint");
    FixtureDesktop {
        control_file: control::control_file(&dir),
        dir,
        media_id: media.id,
        segment_id: segment.id,
        revision: segment.revision,
    }
}

/// A running endpoint over dir/tsubame.db (product rows and jobs in one
/// SQLite file, as the Desktop keeps them), so a test can open the file
/// read-only afterwards. Its directory is removed on drop (best effort: on
/// Windows the endpoint's open connections can keep the file busy).
pub struct FileFixtureDesktop {
    pub dir: PathBuf,
    pub control_file: PathBuf,
    pub db_path: PathBuf,
    pub media_id: i64,
    /// Two Segments in time order, no translation yet:
    /// "晚安。" 1000–2000 ms and "おはよう。" 3000–4000 ms.
    pub segment_ids: Vec<i64>,
    /// A preset voice profile regenerate can use.
    pub voice_profile_id: String,
}

impl Drop for FileFixtureDesktop {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Like [start_fixture_desktop], but file-backed and with a regenerate
/// runner registered. The runner synthesizes with [MockTts] directly; it
/// does not go through tts::adapter_for_provider, so no Provider kind ever
/// selects the mock (docs/19-dubbing-pipeline.md). The spatialize stage is
/// reported as skipped.
pub fn start_file_backed_desktop() -> FileFixtureDesktop {
    let dir = std::env::temp_dir().join(format!("yuzuki-e2e-{}", uuid::Uuid::new_v4().simple()));
    std::fs::create_dir_all(&dir).expect("fixture dir");
    let media_path = dir.join("e2e.wav");
    write_silent_wav(&media_path, 5);
    let db_path = dir.join("tsubame.db");

    let mut db = MediaDb::open(&db_path).expect("file db");
    let media = db
        .import_media(media_path.to_str().expect("utf-8 temp path"))
        .expect("fixture media");
    let cue = |start_ms, end_ms, text: &str| SubtitleCue {
        start_ms,
        end_ms,
        text: text.into(),
    };
    let segment_ids = db
        .replace_segments(
            media.id,
            &[cue(1_000, 2_000, "晚安。"), cue(3_000, 4_000, "おはよう。")],
            "human",
        )
        .expect("fixture segments")
        .into_iter()
        .map(|segment| segment.id)
        .collect();
    let profile = voice::save(
        db.connection(),
        &VoiceProfileInput {
            id: None,
            name: "e2e preset".into(),
            kind: "preset".into(),
            provider_id: "test-support".into(),
            model_id: "mock-tts".into(),
            preset_voice: Some("alloy".into()),
            design_prompt: None,
            language: "en".into(),
            params_json: None,
        },
    )
    .expect("fixture voice profile");

    let jobs = JobStore::open(&db_path).expect("file jobs");
    let db = Arc::new(Mutex::new(db));
    let assets = dir.join("assets");
    std::fs::create_dir_all(&assets).expect("asset dir");
    jobs.register_runner(
        REGENERATE_JOB,
        mock_regenerate_runner(db.clone(), AssetStore::new(assets)),
    );
    let dispatch: Dispatch = Arc::new(move |name, input| {
        let ctx = OperationContext {
            db: &db,
            jobs: &jobs,
        };
        invoke_json(&ctx, name, input)
    });
    control::start(&dir, dispatch).expect("control endpoint");
    FileFixtureDesktop {
        control_file: control::control_file(&dir),
        dir,
        db_path,
        media_id: media.id,
        segment_ids,
        voice_profile_id: profile.id,
    }
}

/// The Desktop's regenerate runner with the TTS adapter replaced by MockTts.
fn mock_regenerate_runner(db: Arc<Mutex<MediaDb>>, store: AssetStore) -> JobRunner {
    Arc::new(move |job: &JobHandle| {
        let input: RegenerateInput = serde_json::from_value(job.input()).map_err(|e| {
            JobError::new(
                "invalid_input",
                false,
                format!("invalid regenerate job input: {e}"),
            )
        })?;
        let profile = {
            let guard = db
                .lock()
                .map_err(|e| JobError::new("db", true, e.to_string()))?;
            voice::get(guard.connection(), &input.voice_profile_id)
                .map_err(|e| JobError::new("db", true, e.to_string()))?
                .ok_or_else(|| JobError::new("not_found", false, "voice profile not found"))?
        };
        let outcome = run_regenerate(
            &db,
            &store,
            &MockTts::new(),
            &profile,
            &SpatialAvailability::Unavailable(
                "spatial_worker_unavailable: test-support fixture".into(),
            ),
            job,
            &input,
        )?;
        serde_json::to_value(outcome).map_err(|e| JobError::new("internal", false, e.to_string()))
    })
}

fn write_silent_wav(path: &Path, seconds: u32) {
    const RATE: u32 = 24_000;
    let spec = hound::WavSpec {
        channels: 1,
        sample_rate: RATE,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).expect("fixture wav");
    for _ in 0..RATE * seconds {
        writer.write_sample(0i16).expect("fixture wav sample");
    }
    writer.finalize().expect("fixture wav finalize");
}
