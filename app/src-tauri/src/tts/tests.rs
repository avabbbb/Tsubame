use super::*;
use crate::jobs::NoopSink;
use crate::subtitle::SubtitleCue;
use crate::voice::VoiceProfileInput;
use std::io::{BufRead, BufReader, Read};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};

fn temp_store() -> AssetStore {
    let dir = std::env::temp_dir().join(format!("tsubame-tts-test-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    AssetStore::new(dir)
}

struct Fixture {
    db: Mutex<MediaDb>,
    store: AssetStore,
    segment: Segment,
    profile: VoiceProfile,
}

fn profile_input(kind: &str) -> VoiceProfileInput {
    VoiceProfileInput {
        id: None,
        name: format!("{kind} voice"),
        kind: kind.into(),
        provider_id: "prov".into(),
        model_id: "tts-1".into(),
        preset_voice: Some("alloy".into()),
        design_prompt: if kind == "design" {
            Some("温柔自然的中文女声，耳语感".into())
        } else {
            None
        },
        language: "zh-Hans".into(),
        params_json: Some(r#"{"speed":1.0}"#.into()),
    }
}

fn add_translated_segment(db: &mut MediaDb, media_path: &str, text: &str) -> Segment {
    let media = db.import_media(media_path).unwrap();
    let rows = db
        .replace_segments(
            media.id,
            &[SubtitleCue {
                start_ms: 1_000,
                end_ms: 2_500,
                text: "おやすみなさい".into(),
            }],
            "import:srt",
        )
        .unwrap();
    let seg = &rows[0];
    db.update_segment(
        seg.id,
        seg.revision,
        &seg.source_text,
        text,
        seg.start_ms,
        seg.end_ms,
    )
    .unwrap()
    .unwrap()
}

fn fixture(kind: &str) -> Fixture {
    let mut db = MediaDb::open(Path::new(":memory:")).unwrap();
    let segment = add_translated_segment(&mut db, "a.mp3", "晚安，好好休息。");
    let profile = voice::save(db.connection(), &profile_input(kind)).unwrap();
    Fixture {
        db: Mutex::new(db),
        store: temp_store(),
        segment,
        profile,
    }
}

fn run<A: TtsAdapter>(
    fx: &Fixture,
    adapter: &A,
    segment_id: i64,
    revision: i64,
    take: u32,
) -> Result<SynthesisOutcome, TtsError> {
    tauri::async_runtime::block_on(run_synthesis(
        &fx.db,
        &fx.store,
        adapter,
        segment_id,
        revision,
        &fx.profile,
        take,
        &NoopSink,
    ))
}

fn key_for(fx: &Fixture, profile: &VoiceProfile, take: u32) -> String {
    let db = fx.db.lock().unwrap();
    let segment = db.get_segment(fx.segment.id).unwrap().unwrap();
    prepare(
        &db,
        segment.id,
        segment.revision,
        profile,
        MOCK_ADAPTER_VERSION,
        take,
    )
    .unwrap()
    .key
}

// ----- cache key -----

#[test]
fn cache_key_ignores_timing_but_tracks_synthesis_inputs() {
    let fx = fixture("design");
    let base = key_for(&fx, &fx.profile, 0);

    // Timing edit: revision changes, key does not.
    {
        let db = fx.db.lock().unwrap();
        let seg = db.get_segment(fx.segment.id).unwrap().unwrap();
        db.update_segment(
            seg.id,
            seg.revision,
            &seg.source_text,
            &seg.translated_text,
            5_000,
            9_000,
        )
        .unwrap()
        .unwrap();
    }
    assert_eq!(key_for(&fx, &fx.profile, 0), base);

    // Whitespace / Unicode-normalization-only differences do not matter.
    assert_eq!(
        normalize_text("  晚安，\n 好好休息。 "),
        "晚安， 好好休息。"
    );
    assert_eq!(normalize_text("e\u{301}"), "\u{e9}");

    // Take, voice, params, model, language all change the key.
    assert_ne!(key_for(&fx, &fx.profile, 1), base);
    let mut p = fx.profile.clone();
    p.design_prompt = "low calm narrator".into();
    assert_ne!(key_for(&fx, &p, 0), base);
    let mut p = fx.profile.clone();
    p.params_json = r#"{"speed":1.1}"#.into();
    assert_ne!(key_for(&fx, &p, 0), base);
    let mut p = fx.profile.clone();
    p.model_id = "tts-2".into();
    assert_ne!(key_for(&fx, &p, 0), base);
    let mut p = fx.profile.clone();
    p.language = "en".into();
    assert_ne!(key_for(&fx, &p, 0), base);
    // Renaming the profile does not.
    let mut p = fx.profile.clone();
    p.name = "renamed".into();
    assert_eq!(key_for(&fx, &p, 0), base);
    // Equivalent params in another key order do not.
    let mut p = fx.profile.clone();
    p.params_json = r#"{ "speed" : 1.0 }"#.into();
    assert_eq!(key_for(&fx, &p, 0), base);

    // Text change does.
    {
        let db = fx.db.lock().unwrap();
        let seg = db.get_segment(fx.segment.id).unwrap().unwrap();
        db.update_segment(
            seg.id,
            seg.revision,
            &seg.source_text,
            "晚安。",
            seg.start_ms,
            seg.end_ms,
        )
        .unwrap()
        .unwrap();
    }
    assert_ne!(key_for(&fx, &fx.profile, 0), base);
}

#[test]
fn cache_key_fields_are_unambiguous() {
    let a = CacheKeyInput {
        provider_id: "ab",
        model_id: "c",
        adapter_version: "v",
        normalized_text: "t",
        language: "",
        voice_profile_hash: "h",
        params_json: "{}",
        take: 0,
    };
    let mut b = a.clone();
    b.provider_id = "a";
    b.model_id = "bc";
    assert_ne!(cache_key(&a), cache_key(&b));
    assert_eq!(cache_key(&a).len(), 64);
}

// ----- orchestration -----

#[test]
fn synthesis_sets_asset_provenance_and_dirty_flags() {
    let fx = fixture("design");
    let adapter = MockTts::new();
    let out = run(&fx, &adapter, fx.segment.id, fx.segment.revision, 0).unwrap();
    assert!(!out.cache_hit);
    assert_eq!(adapter.call_count(), 1);
    assert_eq!(out.segment.tts_provenance, "tts:prov:tts-1");
    assert!(!out.segment.dirty_tts);
    assert!(out.segment.dirty_mix);
    assert_eq!(out.segment.revision, fx.segment.revision + 1);
    assert_eq!(out.asset.sample_rate, 24_000);
    assert_eq!(out.asset.channels, 1);
    assert!(out.asset.duration_ms > 0);
    assert!(Path::new(&out.path).is_file());
    assert!(out.path.contains(&format!(
        "tts/{}/{}.wav",
        &out.asset.key[..2],
        out.asset.key
    )));

    let db = fx.db.lock().unwrap();
    let (key, skip) = segment_dub_fields(db.connection(), fx.segment.id)
        .unwrap()
        .unwrap();
    assert_eq!(key.as_deref(), Some(out.asset.key.as_str()));
    assert!(!skip);
    let segment = db.get_segment(fx.segment.id).unwrap().unwrap();
    assert_eq!(segment.tts_asset_key, key);
    assert!(!segment.dub_skip);
    let resolved = resolve_segment_tts_asset(&db, &fx.store, fx.segment.id)
        .unwrap()
        .unwrap();
    assert_eq!(resolved.duration_ms, out.asset.duration_ms);
    assert_eq!(resolved.path, PathBuf::from(&out.path));
    // Request carried the translation and the design voice.
    let request = adapter.last_request.lock().unwrap().clone().unwrap();
    assert_eq!(request.text, "晚安，好好休息。");
    assert_eq!(request.voice_kind, "design");
    assert!(!request.design_prompt.is_empty());
}

#[test]
fn cache_hit_skips_adapter_and_is_shared_across_sentences() {
    let fx = fixture("preset");
    let adapter = MockTts::new();
    let first = run(&fx, &adapter, fx.segment.id, fx.segment.revision, 0).unwrap();

    // Same request again: no adapter call, no extra revision bump.
    let again = run(&fx, &adapter, fx.segment.id, first.segment.revision, 0).unwrap();
    assert!(again.cache_hit);
    assert_eq!(adapter.call_count(), 1);
    assert_eq!(again.segment.revision, first.segment.revision);

    // A different sentence (other media, other timing) with the same text and
    // voice reuses the clip.
    let other = {
        let mut db = fx.db.lock().unwrap();
        add_translated_segment(&mut db, "b.mp3", "晚安，好好休息。")
    };
    let shared = run(&fx, &adapter, other.id, other.revision, 0).unwrap();
    assert!(shared.cache_hit);
    assert_eq!(shared.asset.key, first.asset.key);
    assert_eq!(adapter.call_count(), 1);

    // A new take is a new clip; the old one stays.
    let take = run(&fx, &adapter, other.id, shared.segment.revision, 1).unwrap();
    assert!(!take.cache_hit);
    assert_ne!(take.asset.key, first.asset.key);
    assert_eq!(adapter.call_count(), 2);
    assert!(Path::new(&first.path).is_file());
}

#[test]
fn corrupted_cache_file_is_resynthesized() {
    let fx = fixture("preset");
    let adapter = MockTts::new();
    let first = run(&fx, &adapter, fx.segment.id, fx.segment.revision, 0).unwrap();
    std::fs::write(&first.path, b"garbage").unwrap();
    let again = run(&fx, &adapter, fx.segment.id, first.segment.revision, 0).unwrap();
    assert!(!again.cache_hit);
    assert_eq!(adapter.call_count(), 2);
    assert_eq!(
        sha256_hex(&std::fs::read(&again.path).unwrap()),
        again.asset.sha256
    );
}

#[test]
fn revision_conflict_is_rejected_before_and_after_synthesis() {
    let fx = fixture("preset");
    let adapter = MockTts::new();
    let err = run(&fx, &adapter, fx.segment.id, fx.segment.revision - 1, 0).unwrap_err();
    assert_eq!(err.code, "revision_conflict");
    assert!(err.to_string().contains("revision conflict"));
    assert_eq!(adapter.call_count(), 0);

    // Sentence changes between prepare and commit.
    let db = fx.db.lock().unwrap();
    let prepared = prepare(
        &db,
        fx.segment.id,
        fx.segment.revision,
        &fx.profile,
        MOCK_ADAPTER_VERSION,
        0,
    )
    .unwrap();
    db.update_segment(
        fx.segment.id,
        fx.segment.revision,
        &fx.segment.source_text,
        &fx.segment.translated_text,
        0,
        900,
    )
    .unwrap()
    .unwrap();
    let err = commit_segment(&db, &prepared, fx.segment.revision).unwrap_err();
    assert_eq!(err.code, "revision_conflict");
    let (key, _) = segment_dub_fields(db.connection(), fx.segment.id)
        .unwrap()
        .unwrap();
    assert!(key.is_none());
}

#[test]
fn refuses_skip_missing_or_stale_translation() {
    let fx = fixture("preset");
    let adapter = MockTts::new();
    let skipped = {
        let db = fx.db.lock().unwrap();
        set_dub_skip(&db, fx.segment.id, fx.segment.revision, true).unwrap()
    };
    assert!(skipped.dirty_mix);
    assert!(skipped.dub_skip);
    let err = run(&fx, &adapter, fx.segment.id, skipped.revision, 0).unwrap_err();
    assert!(err.message.contains("skip"));

    let unskipped = {
        let db = fx.db.lock().unwrap();
        set_dub_skip(&db, fx.segment.id, skipped.revision, false).unwrap()
    };
    // Source edited after translation -> translation is stale.
    let stale = {
        let db = fx.db.lock().unwrap();
        db.update_segment(
            fx.segment.id,
            unskipped.revision,
            "別の文",
            &unskipped.translated_text,
            unskipped.start_ms,
            unskipped.end_ms,
        )
        .unwrap()
        .unwrap()
    };
    assert!(stale.dirty_translation);
    let err = run(&fx, &adapter, fx.segment.id, stale.revision, 0).unwrap_err();
    assert!(err.message.contains("out of date"));

    let empty = {
        let db = fx.db.lock().unwrap();
        db.update_segment(
            fx.segment.id,
            stale.revision,
            &stale.source_text,
            "   ",
            stale.start_ms,
            stale.end_ms,
        )
        .unwrap()
        .unwrap()
    };
    let err = run(&fx, &adapter, fx.segment.id, empty.revision, 0).unwrap_err();
    assert!(err.message.contains("no translation"));
    assert_eq!(adapter.call_count(), 0);
}

struct CancelledSink(AtomicBool);
impl ProgressSink for CancelledSink {
    fn progress(&self, _: &str, _: u64, _: u64) {}
    fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

#[test]
fn cancellation_stops_before_adapter() {
    let fx = fixture("preset");
    let adapter = MockTts::new();
    let sink = CancelledSink(AtomicBool::new(true));
    let err = tauri::async_runtime::block_on(run_synthesis(
        &fx.db,
        &fx.store,
        &adapter,
        fx.segment.id,
        fx.segment.revision,
        &fx.profile,
        0,
        &sink,
    ))
    .unwrap_err();
    assert_eq!(err.code, "cancelled");
    assert_eq!(adapter.call_count(), 0);
}

#[test]
fn adapter_is_selected_by_provider_kind() {
    let mut provider = ProviderConfig {
        id: "p".into(),
        name: "P".into(),
        kind: "openai-compatible".into(),
        execution: "local_server".into(),
        system_managed: false,
        availability: "ready".into(),
        message: String::new(),
        base_url: "http://127.0.0.1:1/v1/".into(),
        model_list_url: String::new(),
        auth_mode: "none".into(),
        secret_ref: None,
        enabled: true,
        last_refresh_at: None,
        last_error: None,
    };
    assert!(is_loopback_url("http://127.0.0.1:1234/v1"));
    assert!(is_loopback_url("http://localhost:8080/v1"));
    assert!(is_loopback_url("http://[::1]:8080/v1"));
    assert!(!is_loopback_url("https://api.example.com/v1"));
    let adapter = adapter_for_provider(&provider, None).unwrap();
    assert_eq!(adapter.adapter_version(), OPENAI_COMPATIBLE_ADAPTER_VERSION);
    provider.kind = "faster-whisper".into();
    assert!(adapter_for_provider(&provider, None).is_err());
}

// ----- OpenAI-compatible request shape (mock HTTP server) -----

struct Captured {
    request_line: String,
    headers: Vec<String>,
    body: Value,
}

fn serve_once(
    response_body: Vec<u8>,
    status: &'static str,
) -> (String, std::thread::JoinHandle<Captured>) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    listener.set_nonblocking(true).unwrap();
    let handle = std::thread::spawn(move || {
        // Bounded accept so a failed client never hangs the test run.
        let deadline = std::time::Instant::now() + Duration::from_secs(15);
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(
                        std::time::Instant::now() < deadline,
                        "client never connected"
                    );
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("accept failed: {e}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream
            .set_read_timeout(Some(Duration::from_secs(15)))
            .unwrap();
        let mut reader = BufReader::new(stream.try_clone().unwrap());
        let mut request_line = String::new();
        reader.read_line(&mut request_line).unwrap();
        let mut headers = Vec::new();
        let mut length = 0usize;
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).unwrap();
            let line = line.trim_end().to_string();
            if line.is_empty() {
                break;
            }
            if let Some(value) = line.to_ascii_lowercase().strip_prefix("content-length:") {
                length = value.trim().parse().unwrap();
            }
            headers.push(line);
        }
        let mut body = vec![0u8; length];
        reader.read_exact(&mut body).unwrap();
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: audio/wav\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            response_body.len()
        );
        stream.write_all(head.as_bytes()).unwrap();
        stream.write_all(&response_body).unwrap();
        stream.flush().unwrap();
        Captured {
            request_line: request_line.trim_end().to_string(),
            headers,
            body: serde_json::from_slice(&body).unwrap(),
        }
    });
    (base, handle)
}

fn request(kind: &str) -> TtsRequest {
    TtsRequest {
        contract_version: TTS_CONTRACT_VERSION,
        model_id: "tts-model".into(),
        text: "晚安。".into(),
        language: "zh-Hans".into(),
        voice_kind: kind.into(),
        preset_voice: "alloy".into(),
        design_prompt: if kind == "design" {
            "soft whisper".into()
        } else {
            String::new()
        },
        params: serde_json::json!({"speed": 0.9, "model": "hijack", "response_format": "mp3"}),
        take: 0,
    }
}

fn call_mock_server(kind: &str, auth: &str) -> (Result<Vec<u8>, TtsError>, Captured) {
    let wav = encode_pcm16_wav(16_000, 1, &[0i16; 1600]);
    let (base, handle) = serve_once(wav, "200 OK");
    let adapter = OpenAiCompatibleTts {
        base_url: base,
        auth_mode: auth.into(),
        secret: Some("sk-test".into()),
        timeout: Duration::from_secs(10),
    };
    let req = request(kind);
    let result = tauri::async_runtime::block_on(adapter.synthesize(&req, &NoopSink));
    (result, handle.join().unwrap())
}

#[test]
fn openai_adapter_sends_preset_voice_without_instructions() {
    let (result, captured) = call_mock_server("preset", "bearer");
    assert!(parse_wav(&result.unwrap()).is_ok());
    assert_eq!(captured.request_line, "POST /v1/audio/speech HTTP/1.1");
    assert!(captured
        .headers
        .iter()
        .any(|h| h.eq_ignore_ascii_case("authorization: Bearer sk-test")));
    let body = captured.body;
    assert_eq!(body["model"], "tts-model");
    assert_eq!(body["input"], "晚安。");
    assert_eq!(body["voice"], "alloy");
    assert_eq!(body["response_format"], "wav");
    assert_eq!(body["speed"], 0.9);
    assert!(body.get("instructions").is_none());
}

#[test]
fn openai_adapter_sends_design_prompt_as_instructions() {
    let (result, captured) = call_mock_server("design", "none");
    assert!(result.is_ok());
    assert!(!captured
        .headers
        .iter()
        .any(|h| h.to_ascii_lowercase().starts_with("authorization")));
    assert_eq!(captured.body["instructions"], "soft whisper");
    assert_eq!(captured.body["model"], "tts-model");
}

#[test]
fn openai_adapter_reports_http_errors_as_retryable_when_transient() {
    let (base, handle) = serve_once(
        b"{\"error\":\"slow down\"}".to_vec(),
        "429 Too Many Requests",
    );
    let adapter = OpenAiCompatibleTts {
        base_url: base,
        auth_mode: "none".into(),
        secret: None,
        timeout: Duration::from_secs(10),
    };
    let req = request("preset");
    let err = tauri::async_runtime::block_on(adapter.synthesize(&req, &NoopSink)).unwrap_err();
    handle.join().unwrap();
    assert_eq!(err.code, "http");
    assert!(err.retryable);
    assert!(err.message.contains("429"));
}

// ----- WAV -----

#[test]
fn wav_parse_reads_format_and_duration() {
    let wav = encode_pcm16_wav(48_000, 2, &vec![0i16; 48_000 * 2 * 3 / 2]);
    let info = parse_wav(&wav).unwrap();
    assert_eq!(info.sample_rate, 48_000);
    assert_eq!(info.channels, 2);
    assert_eq!(info.bits_per_sample, 16);
    assert_eq!(info.frames, 72_000);
    assert_eq!(info.duration_ms, 1_500);
}

#[test]
fn wav_parse_handles_streaming_header_and_extra_chunks() {
    let mut wav = encode_pcm16_wav(24_000, 1, &vec![1i16; 24_000]);
    // Streaming responses declare unknown sizes.
    wav[4..8].copy_from_slice(&u32::MAX.to_le_bytes());
    wav[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
    // Insert an odd-sized LIST chunk before data.
    let mut with_list = wav[..36].to_vec();
    with_list.extend_from_slice(b"LIST");
    with_list.extend_from_slice(&3u32.to_le_bytes());
    with_list.extend_from_slice(b"abc\0");
    with_list.extend_from_slice(&wav[36..]);
    let info = parse_wav(&with_list).unwrap();
    assert_eq!(info.duration_ms, 1_000);

    let (normalized, ninfo) = normalize_wav(&with_list).unwrap();
    assert_eq!(ninfo, info);
    assert_eq!(normalized.len(), 44 + 48_000);
    assert_eq!(le_u32(&normalized, 4) as usize, normalized.len() - 8);
    assert_eq!(le_u32(&normalized, 40), 48_000);
}

#[test]
fn wav_parse_rejects_invalid_audio() {
    assert_eq!(
        parse_wav(b"ID3 mp3 data").unwrap_err().code,
        "invalid_audio"
    );
    let empty = encode_pcm16_wav(24_000, 1, &[]);
    assert!(parse_wav(&empty).is_err());
    let mut bad_align = encode_pcm16_wav(24_000, 1, &[0i16; 100]);
    bad_align[32..34].copy_from_slice(&3u16.to_le_bytes());
    assert!(parse_wav(&bad_align).is_err());
}

// ----- asset store -----

#[test]
fn atomic_write_leaves_no_temp_files_and_rejects_escape() {
    let store = temp_store();
    let key = "ab".repeat(32);
    let rel = AssetStore::relative_path(&key).unwrap();
    assert_eq!(rel, format!("tts/ab/{key}.wav"));
    let path = store.write_atomic(&rel, b"one").unwrap();
    store.write_atomic(&rel, b"two").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"two");
    let entries: Vec<_> = std::fs::read_dir(path.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    assert_eq!(entries, vec![format!("{key}.wav")]);

    assert!(AssetStore::relative_path("../etc").is_err());
    assert!(store.absolute("../outside.wav").is_err());
    assert!(store.absolute("/abs.wav").is_err());
}

// ----- migration -----

#[test]
fn migration_upgrades_an_existing_database() {
    let dir = std::env::temp_dir().join(format!("tsubame-mig-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("old.db");
    {
        // Pre-PR #10 shape: no tts_asset_key / dub_skip / tts tables.
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE media_files (
               id INTEGER PRIMARY KEY AUTOINCREMENT, path TEXT NOT NULL UNIQUE,
               title TEXT NOT NULL, media_type TEXT NOT NULL,
               duration_ms INTEGER NOT NULL DEFAULT 0,
               playback_position INTEGER NOT NULL DEFAULT 0,
               file_size INTEGER NOT NULL DEFAULT 0,
               added_at TEXT NOT NULL DEFAULT (datetime('now')));
             CREATE TABLE segments (
               id INTEGER PRIMARY KEY AUTOINCREMENT, media_id INTEGER NOT NULL,
               start_ms INTEGER NOT NULL, end_ms INTEGER NOT NULL,
               source_text TEXT NOT NULL DEFAULT '', translated_text TEXT NOT NULL DEFAULT '',
               ordinal INTEGER NOT NULL DEFAULT 0, revision INTEGER NOT NULL DEFAULT 0);
             INSERT INTO media_files(path,title,media_type) VALUES('x.mp3','x','audio');
             INSERT INTO segments(media_id,start_ms,end_ms,source_text,translated_text,revision)
               VALUES(1,0,1000,'こんにちは','你好',4);",
        )
        .unwrap();
    }
    for _ in 0..2 {
        let db = MediaDb::open(&path).unwrap();
        let seg = db.get_segment(1).unwrap().unwrap();
        assert_eq!(seg.revision, 4);
        assert_eq!(seg.translated_text, "你好");
        assert_eq!(
            segment_dub_fields(db.connection(), 1).unwrap(),
            Some((None, false))
        );
        assert!(get_asset(db.connection(), "missing").unwrap().is_none());
        assert!(voice::list(db.connection()).unwrap().is_empty());
    }
}

#[test]
fn request_body_sends_only_redacted_sentence_text() {
    let mut req = request("preset");
    req.text = "联系 a.b@mail.example.co.jp 或 090-1234-5678".into();
    let body = OpenAiCompatibleTts::request_body(&req);
    assert_eq!(body["input"].as_str(), Some("联系 ⟦EMAIL1⟧ 或 ⟦PHONE1⟧"));
}
