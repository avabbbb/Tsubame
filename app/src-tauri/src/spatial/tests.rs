use super::*;

const ZEROS: &str = "0000000000000000000000000000000000000000000000000000000000000000";

// ----- canonical JSON + keys -----

#[test]
fn float_repr_matches_python() {
    // Expected strings are Python 3.12 `repr(float)`.
    let cases: &[(f64, &str)] = &[
        (0.9, "0.9"),
        (18000.0, "18000.0"),
        (1e-5, "1e-05"),
        (1e16, "1e+16"),
        (1.5e16, "1.5e+16"),
        (0.0001, "0.0001"),
        (123456789012345.6, "123456789012345.6"),
        (-85.0, "-85.0"),
        (0.1 + 0.2, "0.30000000000000004"),
        (-0.0, "-0.0"),
        (0.0, "0.0"),
        (5e-324, "5e-324"),
        (f64::MAX, "1.7976931348623157e+308"),
        (1234567890123456.0, "1234567890123456.0"),
        (9999999999999998.0, "9999999999999998.0"),
        (0.00012345, "0.00012345"),
        (1.22, "1.22"),
        (0.35, "0.35"),
    ];
    for (value, expected) in cases {
        assert_eq!(py_float_repr(*value), *expected, "repr({value:?})");
    }
}

#[test]
fn canonical_json_matches_python_dumps() {
    // json.dumps(['a"\\\n\x01\x7fé', {'b': 1, 'a': [True, None]}],
    //            sort_keys=True, separators=(",", ":"), ensure_ascii=False)
    let value = json!(["a\"\\\n\u{01}\u{7f}é", {"b": 1, "a": [true, null]}]);
    assert_eq!(
        canonical_json(&value).unwrap(),
        "[\"a\\\"\\\\\\n\\u0001\u{7f}é\",{\"a\":[true,null],\"b\":1}]"
    );
    // Ints stay ints, floats keep a fraction.
    assert_eq!(
        canonical_json(&json!({"i": 20, "f": 20.0, "n": -3})).unwrap(),
        r#"{"f":20.0,"i":20,"n":-3}"#
    );
}

#[test]
fn track_key_payload_matches_the_readme() {
    let payload = json!([
        TRACK_DOMAIN,
        ZEROS,
        "mix",
        default_analysis_params(),
        "spatial-analyze/1"
    ]);
    let expected = format!(
        "[\"tsubame.spatial_track.v1\",\"{ZEROS}\",\"mix\",{{\"activity_below_p90_db\":30.0,\
\"activity_floor_db\":-85.0,\"band_halfwidth_ratio\":1.22,\"band_hi_hz\":18000.0,\
\"band_lo_hz\":100.0,\"bands\":28,\"color_clip_db\":6.0,\"frame_ms\":85,\"hop_ms\":20,\
\"ild_clip_db\":24.0,\"ild_hardpan_db\":12.0,\"itd_hi_hz\":6500.0,\"itd_lo_hz\":180.0,\
\"itd_max_ms\":0.9,\"itd_reliable_coherence\":0.15,\"smoothing\":{{\"color\":19,\"ild\":9,\
\"itd\":7,\"itd_median\":5,\"level\":19}}}},\"spatial-analyze/1\"]"
    );
    assert_eq!(canonical_json(&payload).unwrap(), expected);
}

#[test]
fn pinned_key_vectors() {
    // workers/dubber/spatial/README.md § Cache keys and tests/test_{analyze,render}.py.
    let track =
        spatial_track_key(ZEROS, "mix", &default_analysis_params(), ANALYZER_VERSION).unwrap();
    assert_eq!(
        track,
        "1384a1916325c0effe1fca42d957658fd6f3b783aa22b24801fb6df2d0167923"
    );
    // The reference hash is lower-cased like the worker does.
    let upper = "A".repeat(64);
    assert_eq!(
        spatial_track_key(&upper, "mix", &default_analysis_params(), ANALYZER_VERSION).unwrap(),
        spatial_track_key(
            &"a".repeat(64),
            "mix",
            &default_analysis_params(),
            ANALYZER_VERSION
        )
        .unwrap()
    );
    let window = RenderWindow {
        src_start_ms: 1000,
        src_end_ms: 2500,
        dub_start_ms: 2800,
    };
    let strengths = Strengths::default();
    assert_eq!(
        render_key(
            "tts:abc",
            ZEROS,
            SpatialMode::SourceTrajectory,
            &window,
            &strengths,
            RENDERER_VERSION
        )
        .unwrap(),
        "a77b7081a3038637b6ff0199f11aad0eeaabddfd61f1e89c12c4b9a321afdb4e"
    );
    assert_eq!(
        render_key(
            "tts:abc",
            ZEROS,
            SpatialMode::Concurrent,
            &window,
            &strengths,
            RENDERER_VERSION
        )
        .unwrap(),
        "05fb51ff4c93a080b5d5ab75046f4a3a3493fa3331a85a4cb65974964c1edd8e"
    );
}

#[test]
fn render_key_tracks_only_output_relevant_fields() {
    let w = RenderWindow {
        src_start_ms: 1000,
        src_end_ms: 2000,
        dub_start_ms: 2300,
    };
    let moved = RenderWindow {
        dub_start_ms: 9000,
        ..w
    };
    let key = |mode, window: &RenderWindow, strengths: &Strengths| {
        render_key("tts", "trk", mode, window, strengths, RENDERER_VERSION).unwrap()
    };
    let s = Strengths::default();
    let st = SpatialMode::SourceTrajectory;
    let cc = SpatialMode::Concurrent;
    assert_eq!(key(st, &w, &s), key(st, &moved, &s));
    assert_ne!(key(cc, &w, &s), key(cc, &moved, &s));
    assert_ne!(key(st, &w, &s), key(cc, &w, &s));
    let louder = Strengths { color: 0.5, ..s };
    assert_ne!(key(st, &w, &s), key(st, &w, &louder));
    let longer = RenderWindow {
        src_end_ms: 2001,
        ..w
    };
    assert_ne!(key(st, &w, &s), key(st, &longer, &s));
}

#[test]
fn mode_serde_and_default() {
    assert_eq!(SpatialMode::default(), SpatialMode::SourceTrajectory);
    assert_eq!(
        serde_json::to_value(SpatialMode::Concurrent).unwrap(),
        json!("concurrent")
    );
    let parsed: SpatialMode = serde_json::from_value(json!("source_trajectory")).unwrap();
    assert_eq!(parsed, SpatialMode::SourceTrajectory);
}

// ----- persistence -----

#[test]
fn asset_paths_are_sharded_and_validated() {
    let key = "ab".repeat(32);
    assert_eq!(
        track_relative_path(&key).unwrap(),
        format!("spatial/tracks/ab/{key}.npz")
    );
    assert_eq!(
        render_relative_path(&key).unwrap(),
        format!("spatial/renders/ab/{key}.wav")
    );
    assert!(render_relative_path("../x").is_err());
    assert!(render_relative_path(&"AB".repeat(32)).is_err());
}

#[test]
fn migration_is_idempotent_and_runs_from_media_db_open() {
    let dir = std::env::temp_dir().join(format!("tsubame-spatial-mig-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("t.db");
    for round in 0..2 {
        let db = MediaDb::open(&path).unwrap();
        migrate(db.connection()).unwrap();
        let conn = db.connection();
        let mut stmt = conn.prepare("PRAGMA table_info(spatial_assets)").unwrap();
        let columns = stmt
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(
            columns,
            [
                "key",
                "kind",
                "media_id",
                "segment_id",
                "path",
                "sha256",
                "created_at"
            ]
        );
        if round == 0 {
            let asset = SpatialAsset {
                key: "a".repeat(64),
                kind: KIND_ANALYSIS.into(),
                media_id: 1,
                segment_id: None,
                path: track_relative_path(&"a".repeat(64)).unwrap(),
                sha256: "f".repeat(64),
                created_at: String::new(),
            };
            upsert_asset(conn, &asset).unwrap();
            upsert_asset(conn, &asset).unwrap();
            let bad = conn.execute(
                "INSERT INTO spatial_assets(key,kind,media_id,path,sha256)
                 VALUES('x','other',1,'p','s')",
                [],
            );
            assert!(bad.is_err(), "kind is constrained");
        }
        // The row survives re-opening.
        let row = get_asset(conn, &"a".repeat(64)).unwrap().unwrap();
        assert_eq!(row.kind, KIND_ANALYSIS);
        assert_eq!(row.segment_id, None);
        assert!(!row.created_at.is_empty());
        assert!(!has_renders(conn, 1).unwrap());
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn verified_asset_treats_missing_or_corrupt_files_as_misses() {
    let root = std::env::temp_dir().join(format!("tsubame-spatial-va-{}", uuid::Uuid::new_v4()));
    let store = AssetStore::new(&root);
    let db = MediaDb::open(Path::new(":memory:")).unwrap();
    let conn = db.connection();
    let key = "c".repeat(64);
    let relative = render_relative_path(&key).unwrap();
    let file = store.absolute(&relative).unwrap();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, b"render").unwrap();
    upsert_asset(
        conn,
        &SpatialAsset {
            key: key.clone(),
            kind: KIND_RENDER.into(),
            media_id: 1,
            segment_id: Some(3),
            path: relative,
            sha256: worker::sha256_file(&file).unwrap(),
            created_at: String::new(),
        },
    )
    .unwrap();
    assert!(has_renders(conn, 1).unwrap());
    let (asset, path) = verified_asset(conn, &store, &key, KIND_RENDER)
        .unwrap()
        .unwrap();
    assert_eq!(asset.segment_id, Some(3));
    assert_eq!(path, file);
    assert!(verified_asset(conn, &store, &key, KIND_ANALYSIS)
        .unwrap()
        .is_none());
    std::fs::write(&file, b"tampered").unwrap();
    assert!(verified_asset(conn, &store, &key, KIND_RENDER)
        .unwrap()
        .is_none());
    std::fs::remove_file(&file).unwrap();
    assert!(verified_asset(conn, &store, &key, KIND_RENDER)
        .unwrap()
        .is_none());
    let _ = std::fs::remove_dir_all(&root);
}

// ----- mix selection -----

fn temp_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("tsubame-spatial-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn mix_clip(segment_id: i64, tts: Option<&str>, src: (i64, i64), dub_start_ms: i64) -> MixClip {
    MixClip {
        segment_id,
        path: PathBuf::from(format!("/raw/{segment_id}.wav")),
        dub_start_ms,
        src_start_ms: src.0,
        src_end_ms: src.1,
        expected_duration_ms: None,
        tts_asset_key: tts.map(str::to_string),
    }
}

fn store_render(conn: &Connection, store: &AssetStore, key: &str, segment_id: i64) -> PathBuf {
    let relative = render_relative_path(key).unwrap();
    let file = store.absolute(&relative).unwrap();
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, key.as_bytes()).unwrap();
    upsert_asset(
        conn,
        &SpatialAsset {
            key: key.to_string(),
            kind: KIND_RENDER.into(),
            media_id: 1,
            segment_id: Some(segment_id),
            path: relative,
            sha256: worker::sha256_file(&file).unwrap(),
            created_at: String::new(),
        },
    )
    .unwrap();
    file
}

#[test]
fn mix_uses_a_render_only_for_the_clips_current_key() {
    let root = temp_root("select");
    let store = AssetStore::new(&root);
    let db = MediaDb::open(Path::new(":memory:")).unwrap();
    let conn = db.connection();
    let track = "e".repeat(64);
    let st = SpatialMode::SourceTrajectory;
    let window = RenderWindow {
        src_start_ms: 1000,
        src_end_ms: 2000,
        dub_start_ms: 2100,
    };
    let key = |tts: &str, mode, w: &RenderWindow| {
        render_key(
            tts,
            &track,
            mode,
            w,
            &Strengths::default(),
            RENDERER_VERSION,
        )
        .unwrap()
    };
    let rendered = store_render(conn, &store, &key("tts-a", st, &window), 1);
    let concurrent = store_render(
        conn,
        &store,
        &key("tts-c", SpatialMode::Concurrent, &window),
        4,
    );

    let mut clips = vec![
        // Current key matches (dub start moved: irrelevant in source_trajectory).
        mix_clip(1, Some("tts-a"), (1000, 2000), 5000),
        // Re-synthesized clip: new TTS key → stale render is not used.
        mix_clip(2, Some("tts-a2"), (1000, 2000), 2100),
        // Source window edited → stale render is not used.
        mix_clip(3, Some("tts-a"), (1000, 2001), 2100),
        // Only a concurrent render exists.
        mix_clip(4, Some("tts-c"), (1000, 2000), 2100),
        // No TTS key at all.
        mix_clip(5, None, (1000, 2000), 2100),
    ];
    let swapped = apply_spatial_renders(conn, &store, &mut clips, &track, st).unwrap();
    assert_eq!(swapped, 1);
    assert_eq!(clips[0].path, rendered);
    for clip in &clips[1..] {
        assert_eq!(
            clip.path,
            PathBuf::from(format!("/raw/{}.wav", clip.segment_id))
        );
    }

    // Concurrent mode keys on the dub start too.
    let mut clips = vec![
        mix_clip(4, Some("tts-c"), (1000, 2000), 2100),
        mix_clip(6, Some("tts-c"), (1000, 2000), 2200),
        mix_clip(1, Some("tts-a"), (1000, 2000), 2100),
    ];
    let swapped =
        apply_spatial_renders(conn, &store, &mut clips, &track, SpatialMode::Concurrent).unwrap();
    assert_eq!(swapped, 1);
    assert_eq!(clips[0].path, concurrent);
    assert_eq!(clips[1].path, PathBuf::from("/raw/6.wav"));
    assert_eq!(clips[2].path, PathBuf::from("/raw/1.wav"));

    // A different track (media file changed) matches nothing; a corrupt render is skipped.
    let mut clips = vec![mix_clip(1, Some("tts-a"), (1000, 2000), 2100)];
    assert_eq!(
        apply_spatial_renders(conn, &store, &mut clips, &"f".repeat(64), st).unwrap(),
        0
    );
    std::fs::write(&rendered, b"corrupt").unwrap();
    assert_eq!(
        apply_spatial_renders(conn, &store, &mut clips, &track, st).unwrap(),
        0
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn job_input_shape() {
    let input: SpatialJobInput =
        serde_json::from_value(json!({"op": "render", "segment_id": 7})).unwrap();
    assert!(matches!(
        input,
        SpatialJobInput::Render {
            segment_id: 7,
            mode: SpatialMode::SourceTrajectory
        }
    ));
    assert_eq!(
        serde_json::to_value(SpatialJobInput::Analyze { media_id: 3 }).unwrap(),
        json!({"op": "analyze", "media_id": 3})
    );
}

// ----- real worker -----

/// `python3` (or `TSUBAME_TEST_PYTHON`) with numpy importable; honours
/// `PYTHONPATH` from the environment.
fn python_with_numpy() -> Option<String> {
    let python = crate::worker::test_support::python()?;
    std::process::Command::new(&python)
        .args(["-c", "import numpy"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .map(|_| python)
}

fn write_wav(path: &Path, rate: u32, channels: u16, samples: &[i16]) {
    let spec = hound::WavSpec {
        channels,
        sample_rate: rate,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut writer = hound::WavWriter::create(path, spec).unwrap();
    for sample in samples {
        writer.write_sample(*sample).unwrap();
    }
    writer.finalize().unwrap();
}

/// Deterministic noise in -1..1 (xorshift).
fn noise(n: usize, seed: u64) -> Vec<f64> {
    let mut state = seed;
    (0..n)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 11) as f64 / (1u64 << 53) as f64 * 2.0 - 1.0
        })
        .collect()
}

#[test]
fn worker_analyzes_renders_and_caches() {
    let Some(python) = python_with_numpy() else {
        eprintln!("skipping: python3 with numpy not available");
        return;
    };
    let root = temp_root("worker");
    let store = AssetStore::new(root.join("assets"));

    // Reference: 2 s of 48 kHz stereo, a modulated noise "voice" panned left
    // (right ear quieter and 0.3 ms later).
    let rate = 48_000usize;
    let src = noise(rate * 2, 7);
    let delay = (0.0003 * rate as f64) as usize;
    let mut reference = Vec::with_capacity(src.len() * 2);
    for i in 0..src.len() {
        let env = 0.5 + 0.5 * (i as f64 / rate as f64 * 3.0 * std::f64::consts::TAU).sin();
        let right = if i >= delay { src[i - delay] } else { 0.0 };
        reference.push((src[i] * env * 12_000.0) as i16);
        reference.push((right * env * 4_000.0) as i16);
    }
    let reference_path = root.join("reference.wav");
    write_wav(&reference_path, rate as u32, 2, &reference);

    // TTS clip: 0.8 s of 24 kHz mono noise.
    let clip: Vec<i16> = noise(19_200, 11)
        .iter()
        .map(|x| (x * 8_000.0) as i16)
        .collect();
    let tts_key = "d".repeat(64);
    let tts_rel = tts::AssetStore::relative_path(&tts_key).unwrap();
    let tts_file = store.absolute(&tts_rel).unwrap();
    std::fs::create_dir_all(tts_file.parent().unwrap()).unwrap();
    write_wav(&tts_file, 24_000, 1, &clip);

    let mut db = MediaDb::open(&root.join("t.db")).unwrap();
    let media = db.import_media(reference_path.to_str().unwrap()).unwrap();
    let segment = db
        .replace_segments(
            media.id,
            &[crate::subtitle::SubtitleCue {
                start_ms: 200,
                end_ms: 1500,
                text: "こんにちは".into(),
            }],
            "import:srt",
        )
        .unwrap()
        .remove(0);
    db.connection()
        .execute(
            "INSERT INTO tts_assets(key,path,sha256,duration_ms,sample_rate,channels,provider_id,model_id)
             VALUES(?1,?2,?3,800,24000,1,'mock','mock')",
            params![tts_key, tts_rel, worker::sha256_file(&tts_file).unwrap()],
        )
        .unwrap();
    db.connection()
        .execute(
            "UPDATE segments SET tts_asset_key=?1 WHERE id=?2",
            params![tts_key, segment.id],
        )
        .unwrap();
    let db = Mutex::new(db);

    let mut spec = WorkerSpec::python(python, worker_script());
    spec.hello_timeout = std::time::Duration::from_secs(30);
    let jobs = crate::jobs::JobStore::open_in_memory().unwrap();
    let run = |mode: SpatialMode| {
        let id = jobs
            .create(NewJob::new(SPATIAL_JOB, media.id).segment(segment.id))
            .unwrap()
            .id;
        let handle = jobs.start(&id).unwrap();
        let outcome = spatialize(&db, &store, &spec, &handle, segment.id, mode);
        handle.succeed(json!({})).unwrap();
        outcome
    };

    let first = run(SpatialMode::SourceTrajectory).expect("render through the worker");
    assert!(!first.cache_hit);
    assert!(!first.analysis_cache_hit);
    assert!(first.report.is_some());
    let (reference_sha, track_key) = current_track_key(&reference_path).unwrap();
    assert_eq!(first.spatial_track_key, track_key);
    assert_eq!(
        first.render_key,
        render_key(
            &tts_key,
            &track_key,
            SpatialMode::SourceTrajectory,
            &RenderWindow {
                src_start_ms: 200,
                src_end_ms: 1500,
                dub_start_ms: 0
            },
            &Strengths::default(),
            RENDERER_VERSION
        )
        .unwrap()
    );
    assert_eq!(reference_sha, worker::sha256_file(&reference_path).unwrap());
    let reader = hound::WavReader::open(&first.path).unwrap();
    assert_eq!(reader.spec().channels, 2);
    assert_eq!(reader.spec().sample_rate, 24_000);
    assert_eq!(
        reader.duration() as usize,
        clip.len(),
        "never time-stretched"
    );
    drop(reader);
    {
        let guard = db.lock().unwrap();
        let conn = guard.connection();
        let analysis = get_asset(conn, &track_key).unwrap().unwrap();
        assert_eq!(analysis.kind, KIND_ANALYSIS);
        assert_eq!(analysis.path, track_relative_path(&track_key).unwrap());
        let render = get_asset(conn, &first.render_key).unwrap().unwrap();
        assert_eq!(render.segment_id, Some(segment.id));
    }
    assert!(
        !store.root.join("spatial/tmp").exists()
            || std::fs::read_dir(store.root.join("spatial/tmp"))
                .unwrap()
                .next()
                .is_none(),
        "scratch dirs are removed"
    );

    // Second run: both caches hit, the worker is not needed.
    let second = run(SpatialMode::SourceTrajectory).unwrap();
    assert!(second.cache_hit && second.analysis_cache_hit);
    assert_eq!(second.path, first.path);

    // Concurrent mode reuses the analysis and keys on the placed dub start.
    let concurrent = run(SpatialMode::Concurrent).unwrap();
    assert!(!concurrent.cache_hit && concurrent.analysis_cache_hit);
    assert_ne!(concurrent.render_key, first.render_key);

    // Mix picks the source_trajectory render for the placed clip.
    let guard = db.lock().unwrap();
    let mut inputs =
        crate::mix::export_inputs(guard.connection(), media.id, &PlacementParams::default())
            .unwrap();
    assert_eq!(inputs.clips.len(), 1);
    let swapped = apply_spatial_renders(
        guard.connection(),
        &store,
        &mut inputs.clips,
        &track_key,
        SpatialMode::SourceTrajectory,
    )
    .unwrap();
    assert_eq!(swapped, 1);
    assert_eq!(inputs.clips[0].path, PathBuf::from(&first.path));
    drop(guard);
    let _ = std::fs::remove_dir_all(&root);
}
