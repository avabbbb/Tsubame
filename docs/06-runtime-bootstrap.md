# Runtime Bootstrap and Desktop Packaging

## User promise

A normal user installs YUZUKI Desktop and does not manually configure developer environments.

Do not require:
- system Python;
- Node/npm;
- Rust/cargo;
- Visual Studio toolchain;
- manual FFmpeg PATH setup;
- global CUDA package setup.

## Distribution shape

Initial production target: Windows x64.

```text
YUZUKI installer
  └─ Desktop core
      ├─ Tauri app
      ├─ bootstrap/runtime manager
      ├─ bundled minimal media helper(s)
      └─ runtime catalog + trusted keys
```

Heavy AI runtimes/models are separate on-demand packs.

## App-owned storage

Conceptual layout:

```text
YUZUKIData/
├─ db/
├─ media/
├─ cache/
├─ runtimes/
│  ├─ asr-faster-whisper/<version>/
│  ├─ asr-sensevoice/<version>/
│  └─ tts-indextts/<version>/
├─ models/
├─ jobs/
└─ logs/
```

No installation writes packages into system Python or user-global package managers.

## Runtime Pack

A runtime pack manifest declares:

- `id`
- `version`
- supported OS/arch;
- capabilities;
- download URLs/mirrors;
- compressed/unpacked size;
- SHA-256;
- entrypoint;
- health probe;
- dependencies on other packs;
- license/notices;
- optional hardware requirements.

## Model Pack

Models are versioned independently from runtimes.

A selected ProcessingProfile is resolved into:

```text
required capabilities
 → provider/model selection
 → runtime requirements
 → missing runtime/model packs
 → install plan
```

The UI and Skill can show this plan before downloading.

## Install transaction

1. download to unique temporary path;
2. support resumable/range downloads when source allows;
3. verify size/hash/signature policy;
4. unpack into staging;
5. run validation/health probe;
6. atomically activate version;
7. keep previous known-good version until activation succeeds.

Interrupted installs are recoverable.

## GitHub Releases

The repository may publish:
- desktop installers;
- bootstrap metadata;
- runtime manifests;
- small runtime packs where appropriate.

Large third-party model assets should use their lawful canonical hosts/mirrors when redistribution is not appropriate.

## Updates

Desktop app updates and model/runtime updates are separate channels.

Updating YUZUKI must not force re-downloading multi-GB models when compatible packs already exist.

## Skill bootstrap

The YUZUKI Skill may:
- detect an existing installation;
- download a trusted Desktop release/bootstrap helper when absent;
- run `doctor`;
- ask the Desktop/runtime manager to install missing packs.

The Skill must not reproduce environment-management logic independently. Runtime installation logic belongs to YUZUKI.

## Implementation (PR #12)

Code: app/src-tauri/src/runtime.rs, tests in app/src-tauri/src/runtime/tests.rs. UI: app/src/RuntimePacksCard.tsx, shown in Settings for system-managed pack providers and in the ASR panel while the selected pack provider is not ready.

### On-disk layout

Everything lives under the app data directory, in runtime/:

    runtime/
      active.json             pack id -> active version (+ previous known-good)
      staging/<uuid>.part     download in progress (Range-resumable)
      staging/<uuid>.json     pack/version/sha256 the .part belongs to
      staging/<uuid>.dir/     unpacked, not yet probed/activated
      packs/<id>/<version>/   activated; never modified in place

### Manifest fields

Each built-in pack declares: id, version, kind (runtime or model), platform (<os>-<arch> as Rust std::env::consts spells it, or any), format (zip or file), sources (tried in order; every source serves identical bytes, so a partial download resumes against the next), size, sha256, unpacked_size, entry (executable or model file inside the version directory), license, requires (pack ids), optional hardware.

First real packs: whisper.cpp v1.9.2 (whisper-bin-x64.zip, entry Release/whisper-cli.exe, probe: --help exits 0), whisper.cpp.model.base (ggml-base.bin) and whisper.cpp.model.small (ggml-small.bin). Model sources are huggingface.co first, hf-mirror.com second, same sha256.

### Transaction

download -> size + sha256 -> traversal-safe unpack -> probe -> rename into packs/ -> rewrite active.json (tmp + rename). Any failure before the active.json rename leaves the previous active version intact. Retryable failures keep the .part so the next attempt resumes. Installs run as runtime.install jobs (media_id 0) with stages download, verify, unpack, probe, activate, and support cancel and retry through the Jobs API.

### Commands

- runtime_install_plan(provider_id, model_id?): items, missing ids (dependency-first), download/required bytes, free bytes, fits, ready, blocker.
- install_runtime_packs(provider_id, model_id?): submits the install job; refuses when the plan has a blocker.
- runtime_doctor(): per pack state (active, broken, partial, missing, unsupported), active/previous version, update availability, staged bytes, probe report, developer override.

### Developer overrides

TSUBAME_WHISPER_CPP_BIN and TSUBAME_WHISPER_CPP_MODEL win over active packs. They are always reported (doctor output, ASR result notes) and are never a silent fallback.
