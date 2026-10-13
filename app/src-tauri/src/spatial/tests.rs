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
