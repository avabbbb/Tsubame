//! Install transaction fixtures: tiny in-memory zips, injected fetcher/prober.

use super::*;
use std::io::Cursor;
use zip::write::SimpleFileOptions;

fn temp_root(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("yuzuki-runtime-{name}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    for (name, data) in entries {
        writer.start_file(name.to_string(), options).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn sha(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn zip_pack(id: &str, version: &str, bytes: &[u8], unpacked: u64) -> Pack {
    Pack {
        id: id.into(),
        version: version.into(),
        kind: PackKind::Runtime,
        platform: "any".into(),
        format: PackFormat::Zip,
        sources: vec!["mem:fixture".into()],
        size: bytes.len() as u64,
        sha256: sha(bytes),
        unpacked_size: unpacked,
        entry: "Release/tool.exe".into(),
        license: "MIT".into(),
        requires: vec![],
        hardware: None,
    }
}

fn file_pack(id: &str, entry: &str, bytes: &[u8]) -> Pack {
    Pack {
        kind: PackKind::Model,
        format: PackFormat::File,
        entry: entry.into(),
        unpacked_size: bytes.len() as u64,
        ..zip_pack(id, "1", bytes, 0)
    }
}

struct FailingReader;

impl Read for FailingReader {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(
            io::ErrorKind::ConnectionReset,
            "simulated drop",
        ))
    }
}

/// Serves data honouring offsets; optionally drops the connection once
/// after fail_after bytes. Records every requested offset.
struct MemFetcher {
    data: Vec<u8>,
    fail_after: Mutex<Option<usize>>,
    offsets: Mutex<Vec<u64>>,
}

impl MemFetcher {
    fn new(data: Vec<u8>) -> Self {
        Self {
            data,
            fail_after: Mutex::new(None),
            offsets: Mutex::new(Vec::new()),
        }
    }
}

impl Fetcher for MemFetcher {
    fn open(&self, _url: &str, offset: u64) -> Result<Opened, String> {
        self.offsets.lock().unwrap().push(offset);
        let start = (offset as usize).min(self.data.len());
        let tail = self.data[start..].to_vec();
        let reader: Box<dyn Read + Send> = match self.fail_after.lock().unwrap().take() {
            Some(n) => Box::new(Cursor::new(tail[..n].to_vec()).chain(FailingReader)),
            None => Box::new(Cursor::new(tail)),
        };
        Ok(Opened {
            start: start as u64,
            reader,
        })
    }
}

fn ok_probe(_pack: &Pack, _dir: &Path) -> Result<String, String> {
    Ok("ok".into())
}

fn no_cancel() -> bool {
    false
}

fn install(
    root: &Path,
    fetcher: &dyn Fetcher,
    pack: &Pack,
    prober: &Prober,
) -> Result<ActiveEntry, InstallError> {
    Installer {
        root,
        fetcher,
        prober,
    }
    .install(pack, &no_cancel, &mut |_, _, _| {})
}

fn staging_entries(root: &Path) -> usize {
    fs::read_dir(root.join("staging"))
        .map(|entries| entries.count())
        .unwrap_or(0)
}

fn part_len(root: &Path) -> u64 {
    fs::read_dir(root.join("staging"))
        .unwrap()
        .flatten()
        .map(|entry| entry.path())
        .find(|path| path.extension().and_then(|e| e.to_str()) == Some("part"))
        .map(|path| fs::metadata(path).unwrap().len())
        .unwrap_or(0)
}

fn file_url(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if text.starts_with('/') {
        format!("file:{SLASHES}{text}")
    } else {
        format!("file:{SLASHES}/{text}")
    }
}

#[test]
fn installs_and_activates_zip_pack() {
    let root = temp_root("ok");
    let bytes = zip_bytes(&[("Release/tool.exe", b"bin"), ("Release/lib.dll", b"dll")]);
    let pack = zip_pack("t", "1", &bytes, 6);
    let entry = install(&root, &MemFetcher::new(bytes), &pack, &ok_probe).unwrap();
    assert_eq!(entry.version, "1");
    assert_eq!(read_active(&root).packs["t"], entry);
    assert_eq!(
        fs::read(pack_dir(&root, "t", "1").join("Release/tool.exe")).unwrap(),
        b"bin"
    );
    assert_eq!(
        active_entry_path(&root, "t"),
        Some(pack_dir(&root, "t", "1").join("Release/tool.exe"))
    );
    assert_eq!(staging_entries(&root), 0);
}

#[test]
fn hash_mismatch_does_not_activate() {
    let root = temp_root("hash");
    let bytes = zip_bytes(&[("Release/tool.exe", b"bin")]);
    let mut pack = zip_pack("t", "1", &bytes, 3);
    pack.sha256 = "00".repeat(32);
    let err = install(&root, &MemFetcher::new(bytes), &pack, &ok_probe).unwrap_err();
    assert_eq!(err.code, "hash_mismatch");
    assert!(read_active(&root).packs.is_empty());
    assert!(!pack_dir(&root, "t", "1").exists());
    // Corrupt bytes are discarded, never resumed from.
    assert_eq!(staging_entries(&root), 0);
}

#[test]
fn path_traversal_is_rejected() {
    let root = temp_root("traversal");
    let bytes = zip_bytes(&[("Release/tool.exe", b"bin"), ("../escape.txt", b"x")]);
    let pack = zip_pack("t", "1", &bytes, 4);
    let err = install(&root, &MemFetcher::new(bytes), &pack, &ok_probe).unwrap_err();
    assert_eq!(err.code, "unsafe_archive");
    assert!(!err.retryable);
    assert!(!root.join("staging/escape.txt").exists());
    assert!(!root.join("escape.txt").exists());
    assert!(read_active(&root).packs.is_empty());
    assert!(!pack_dir(&root, "t", "1").exists());
}

#[test]
fn safe_relative_rejects_escapes() {
    for bad in [
        "../a",
        "a/../../b",
        "/abs",
        "C:/win",
        "c:evil",
        "a\\..\\..\\b",
        "",
        "./",
    ] {
        assert_eq!(
            safe_relative(bad).unwrap_err().code,
            "unsafe_archive",
            "{bad:?}"
        );
    }
    assert_eq!(
        safe_relative("Release/whisper-cli.exe").unwrap(),
        PathBuf::from("Release").join("whisper-cli.exe")
    );
}

#[test]
fn unpacked_size_over_manifest_is_rejected() {
    let root = temp_root("bomb");
    let bytes = zip_bytes(&[("Release/tool.exe", &[1u8; 4096])]);
    let pack = zip_pack("t", "1", &bytes, 100);
    let err = install(&root, &MemFetcher::new(bytes), &pack, &ok_probe).unwrap_err();
    assert_eq!(err.code, "unsafe_archive");
    assert!(read_active(&root).packs.is_empty());
}

#[test]
fn interrupted_download_resumes_from_part() {
    let root = temp_root("resume");
    let payload = vec![7u8; 200_000];
    let bytes = zip_bytes(&[("Release/tool.exe", &payload)]);
    let pack = zip_pack("t", "1", &bytes, 200_000);
    let fetcher = MemFetcher::new(bytes);
    fetcher.fail_after.lock().unwrap().replace(100_000);

    let err = install(&root, &fetcher, &pack, &ok_probe).unwrap_err();
    assert_eq!(err.code, "download_failed");
    assert!(err.retryable);
    assert_eq!(part_len(&root), 100_000);
    assert!(read_active(&root).packs.is_empty());

    install(&root, &fetcher, &pack, &ok_probe).unwrap();
    assert_eq!(fetcher.offsets.lock().unwrap().clone(), vec![0, 100_000]);
    assert_eq!(
        fs::read(pack_dir(&root, "t", "1").join("Release/tool.exe")).unwrap(),
        payload
    );
    assert_eq!(staging_entries(&root), 0);
}

#[test]
fn cancel_keeps_staging_slot_for_resume() {
    let root = temp_root("cancel");
    let bytes = zip_bytes(&[("Release/tool.exe", b"bin")]);
    let pack = zip_pack("t", "1", &bytes, 3);
    let fetcher = MemFetcher::new(bytes);
    let err = Installer {
        root: &root,
        fetcher: &fetcher,
        prober: &ok_probe,
    }
    .install(&pack, &|| true, &mut |_, _, _| {})
    .unwrap_err();
    assert_eq!(err.code, "cancelled");
    assert_eq!(JobError::from(err).code, "cancelled");
    assert_eq!(staged_matches(&root.join("staging"), &pack).len(), 1);
    assert!(read_active(&root).packs.is_empty());
}

#[test]
fn probe_failure_keeps_previous_version_active() {
    let root = temp_root("rollback");
    let v1 = zip_bytes(&[("Release/tool.exe", b"one")]);
    let v2 = zip_bytes(&[("Release/tool.exe", b"two")]);
    install(
        &root,
        &MemFetcher::new(v1.clone()),
        &zip_pack("t", "1", &v1, 3),
        &ok_probe,
    )
    .unwrap();

    let failing = |pack: &Pack, _dir: &Path| -> Result<String, String> {
        if pack.version == "2" {
            Err("boom".into())
        } else {
            Ok("ok".into())
        }
    };
    let err = install(
        &root,
        &MemFetcher::new(v2.clone()),
        &zip_pack("t", "2", &v2, 3),
        &failing,
    )
    .unwrap_err();
    assert_eq!(err.code, "probe_failed");
    assert_eq!(read_active(&root).packs["t"].version, "1");
    assert_eq!(
        fs::read(pack_dir(&root, "t", "1").join("Release/tool.exe")).unwrap(),
        b"one"
    );
    assert!(!pack_dir(&root, "t", "2").exists());
    assert_eq!(staging_entries(&root), 0);
}

#[test]
fn keeps_one_previous_known_good_version() {
    let root = temp_root("previous");
    for version in ["1", "2", "3"] {
        let bytes = zip_bytes(&[("Release/tool.exe", version.as_bytes())]);
        let pack = zip_pack("t", version, &bytes, 1);
        install(&root, &MemFetcher::new(bytes), &pack, &ok_probe).unwrap();
    }
    let index = read_active(&root);
    let active = &index.packs["t"];
    assert_eq!(active.version, "3");
    assert_eq!(active.previous.as_deref(), Some("2"));
    assert!(pack_dir(&root, "t", "2").exists());
    assert!(!pack_dir(&root, "t", "1").exists());
}

#[test]
fn reinstall_same_version_replaces_in_place() {
    let root = temp_root("repair");
    let bytes = zip_bytes(&[("Release/tool.exe", b"bin")]);
    let pack = zip_pack("t", "1", &bytes, 3);
    install(&root, &MemFetcher::new(bytes.clone()), &pack, &ok_probe).unwrap();
    fs::remove_file(pack_dir(&root, "t", "1").join("Release/tool.exe")).unwrap();
    install(&root, &MemFetcher::new(bytes), &pack, &ok_probe).unwrap();
    assert!(pack_dir(&root, "t", "1").join("Release/tool.exe").exists());
    assert_eq!(read_active(&root).packs["t"].previous, None);
}

#[test]
fn falls_back_to_next_source() {
    let root = temp_root("sources");
    let model = b"model bytes".to_vec();
    let file = root.join("fixture.bin");
    fs::write(&file, &model).unwrap();
    let mut pack = file_pack("m", "model.bin", &model);
    pack.sources = vec![file_url(&root.join("missing.bin")), file_url(&file)];
    let fetcher = HttpFetcher::new().unwrap();
    install(&root, &fetcher, &pack, &default_probe).unwrap();
    assert_eq!(
        fs::read(pack_dir(&root, "m", "1").join("model.bin")).unwrap(),
        model
    );
}

#[test]
fn plan_counts_only_missing_packs() {
    let root = temp_root("plan");
    let runtime_zip = zip_bytes(&[("Release/tool.exe", b"bin")]);
    let runtime = zip_pack("rt", "1", &runtime_zip, 3);
    let mut model = file_pack("m", "m.bin", b"weights");
    model.requires = vec!["rt".into()];
    let catalog = vec![model.clone(), runtime.clone()];

    let plan = plan_packs(Some(&root), &catalog, &["m"], "p", "").unwrap();
    assert_eq!(plan.missing, vec!["rt", "m"]);
    assert_eq!(plan.download_bytes, runtime.size + model.size);
    assert_eq!(plan.required_bytes, runtime.size + 3 + model.size);
    assert!(!plan.ready);

    install(&root, &MemFetcher::new(runtime_zip), &runtime, &ok_probe).unwrap();
    let plan = plan_packs(Some(&root), &catalog, &["m"], "p", "").unwrap();
    assert_eq!(plan.missing, vec!["m"]);
    assert_eq!(plan.download_bytes, model.size);
    assert!(plan.items[0].installed);
}

#[test]
fn unsupported_platform_blocks_plan_and_install() {
    let root = temp_root("platform");
    let bytes = zip_bytes(&[("Release/tool.exe", b"bin")]);
    let mut pack = zip_pack("t", "1", &bytes, 3);
    pack.platform = "plan9-mips".into();
    let plan = plan_packs(Some(&root), &[pack.clone()], &["t"], "p", "").unwrap();
    assert!(plan.blocker.is_some());
    let err = install(&root, &MemFetcher::new(bytes), &pack, &ok_probe).unwrap_err();
    assert_eq!(err.code, "unsupported_platform");
}

#[test]
fn builtin_catalog_is_consistent() {
    let catalog = builtin_catalog();
    for pack in &catalog {
        assert_eq!(pack.sha256.len(), 64, "{}", pack.id);
        assert!(
            pack.sources.iter().all(|s| s.starts_with("https:")),
            "{}",
            pack.id
        );
        safe_relative(&pack.entry).unwrap();
    }
    assert_eq!(catalog.len(), 3);
    assert!(catalog[0].sources[0].ends_with("/runtime.faster-whisper-1.2.1-1-windows-x86_64.zip"));
    assert_eq!(
        catalog[1].requires,
        vec![FASTER_WHISPER_RUNTIME.to_string()]
    );
    assert_eq!(catalog[1].hardware.as_deref(), Some(HARDWARE_NVIDIA_CUDA));
    let cpu = provider_pack_ids_for("local.faster-whisper", "large-v3-turbo", false).unwrap();
    assert_eq!(cpu, vec![FASTER_WHISPER_RUNTIME]);
    let ids = provider_pack_ids_for("local.faster-whisper", "large-v3-turbo", true).unwrap();
    assert_eq!(ids, vec![FASTER_WHISPER_RUNTIME, FASTER_WHISPER_CUDA]);
    assert_eq!(expand(&catalog, &ids).unwrap().len(), 2);
    assert!(catalog[2]
        .sources
        .last()
        .unwrap()
        .ends_with("/runtime-funasr-1.4.16-1/runtime.funasr-1.4.16-1-windows-x86_64.zip"));
    assert!(catalog[2].requires.is_empty() && catalog[2].hardware.is_none());
    for nvidia in [false, true] {
        let ids = provider_pack_ids_for("local.sensevoice", "iic/SenseVoiceSmall", nvidia).unwrap();
        assert_eq!(ids, vec![FUNASR_RUNTIME]);
    }
    assert_eq!(override_env_for(FUNASR_RUNTIME), Some(ENV_FUNASR_PYTHON));
}

#[test]
fn doctor_reports_state_override_and_probe() {
    let root = temp_root("doctor");
    let bytes = zip_bytes(&[("Release/tool.exe", b"bin")]);
    let mut active = zip_pack("t", "1", &bytes, 3);
    install(&root, &MemFetcher::new(bytes), &active, &ok_probe).unwrap();
    active.version = "2".into();
    let missing = zip_pack(FASTER_WHISPER_RUNTIME, "1.2.1-1", b"py", 2);
    let env =
        |name: &str| (name == ENV_FASTER_WHISPER_PYTHON).then(|| "Z:/nope/python.exe".to_string());

    let report = doctor(Some(&root), &[active, missing], &env, &ok_probe);
    assert_eq!(report[0].state, "active");
    assert_eq!(report[0].active_version.as_deref(), Some("1"));
    assert!(report[0].update_available);
    assert!(report[0].env_override.is_none());
    assert_eq!(report[0].probe.as_ref().map(|p| p.ok), Some(true));
    assert_eq!(report[1].state, "missing");
    let env_override = report[1].env_override.as_ref().unwrap();
    assert_eq!(env_override.name, ENV_FASTER_WHISPER_PYTHON);
    assert!(!env_override.exists);
}

/// Job runners are plain std threads with no Tokio context. Network-bound,
/// so opt in: YUZUKI_NET_TEST_URL=<any http(s) URL> cargo test -- --ignored
#[test]
#[ignore]
fn http_fetcher_runs_on_plain_thread() {
    use std::io::Read;
    let url = std::env::var("YUZUKI_NET_TEST_URL").expect("YUZUKI_NET_TEST_URL");
    std::thread::spawn(move || {
        let fetcher = super::HttpFetcher::new().expect("client");
        let mut opened = super::Fetcher::open(&fetcher, &url, 0).expect("open");
        let mut buf = [0u8; 4096];
        let n = opened.reader.read(&mut buf).expect("read");
        assert!(n > 0);
        drop(opened);
        drop(fetcher);
    })
    .join()
    .expect("fetcher thread panicked");
}

#[test]
fn faster_whisper_python_prefers_override_then_pack() {
    let root = temp_root("fw-python");
    let no_env = |_: &str| -> Option<String> { None };
    let err = resolve_faster_whisper_python_in(Some(&root), &no_env).unwrap_err();
    assert!(err.contains("faster-whisper runtime pack"), "{err}");
    assert!(resolve_faster_whisper_python_in(None, &no_env).is_err());

    let env =
        |name: &str| (name == ENV_FASTER_WHISPER_PYTHON).then(|| "D:/dev/python.exe".to_string());
    let python = resolve_faster_whisper_python_in(Some(&root), &env).unwrap();
    assert_eq!(python, PathBuf::from("D:/dev/python.exe"));
    assert_eq!(
        override_env_for(FASTER_WHISPER_RUNTIME),
        Some(ENV_FASTER_WHISPER_PYTHON)
    );
}

fn active_entry(version: &str, entry: &str) -> ActiveEntry {
    ActiveEntry {
        version: version.into(),
        entry: entry.into(),
        sha256: "00".into(),
        previous: None,
    }
}

#[test]
fn retired_packs_are_removed_and_others_kept() {
    let root = temp_root("retired");
    let layout = [
        ("whisper.cpp", "1.9.2", "Release/whisper-cli.exe"),
        ("whisper.cpp.model.small", "1", "ggml-small.bin"),
        (FASTER_WHISPER_RUNTIME, "1.2.1-1", "python.exe"),
    ];
    let mut index = ActiveIndex::default();
    for (id, version, entry) in layout {
        let file = pack_dir(&root, id, version).join(entry);
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, b"x").unwrap();
        index.packs.insert(id.into(), active_entry(version, entry));
    }
    write_active(&root, &index).unwrap();

    let removed = remove_retired_packs(&root).unwrap();
    assert_eq!(removed, vec!["whisper.cpp", "whisper.cpp.model.small"]);
    assert!(!root.join("packs").join("whisper.cpp").exists());
    assert!(!root.join("packs").join("whisper.cpp.model.small").exists());
    assert!(pack_dir(&root, FASTER_WHISPER_RUNTIME, "1.2.1-1")
        .join("python.exe")
        .exists());
    let active = read_active(&root);
    assert_eq!(
        active.packs.keys().collect::<Vec<_>>(),
        vec![FASTER_WHISPER_RUNTIME]
    );

    // Idempotent: a second startup finds nothing to remove.
    assert!(remove_retired_packs(&root).unwrap().is_empty());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn remove_pack_rejects_path_like_ids() {
    let root = temp_root("remove-invalid");
    for id in ["", ".", "..", "a/b", "a\\b"] {
        let err = remove_pack(&root, id).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{id:?}");
    }
    // A missing pack is not an error, just nothing removed.
    assert!(!remove_pack(&root, "never-installed").unwrap());
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn mirror_source_maps_dirs_and_urls_to_pack_files() {
    let file = "runtime.faster-whisper-1.2.1-1-windows-x86_64.zip";
    let win = mirror_source(r"H:\data\packs\", file).unwrap();
    assert_eq!(win, format!("file:///H:/data/packs/{file}"));
    assert_eq!(file_url_path(&win).unwrap(), std::path::PathBuf::from(format!("H:/data/packs/{file}")));
    assert_eq!(mirror_source("/srv/packs", file).unwrap(), format!("file:///srv/packs/{file}"));
    assert_eq!(
        mirror_source("https://mirror.example/packs/", file).unwrap(),
        format!("https://mirror.example/packs/{file}")
    );
    assert!(mirror_source("  ", file).is_none());
}

#[test]
fn cuda_dir_override_wins_over_missing_pack() {
    let env = |name: &str| (name == ENV_FW_CUDA_DIR).then(|| "H:/cuda".to_string());
    assert_eq!(resolve_faster_whisper_cuda_dir_in(None, &env), Some(PathBuf::from("H:/cuda")));
    let none = |_: &str| None;
    assert_eq!(resolve_faster_whisper_cuda_dir_in(None, &none), None);
}

#[test]
fn funasr_python_prefers_override_then_pack() {
    let root = std::env::temp_dir().join("yuzuki-funasr-resolve-test-empty");
    let no_env = |_: &str| -> Option<String> { None };
    let err = resolve_funasr_python_in(Some(&root), &no_env).unwrap_err();
    assert!(err.contains("FunASR runtime pack"), "{err}");
    assert!(resolve_funasr_python_in(None, &no_env).is_err());

    let env = |name: &str| (name == ENV_FUNASR_PYTHON).then(|| "D:/dev/python.exe".to_string());
    let python = resolve_funasr_python_in(Some(&root), &env).unwrap();
    assert_eq!(python, PathBuf::from("D:/dev/python.exe"));
}
