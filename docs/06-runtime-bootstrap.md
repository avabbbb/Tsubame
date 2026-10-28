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

Each built-in pack declares: id, version, kind (runtime, model or library), platform (<os>-<arch> as Rust std::env::consts spells it, or any), format (zip or file), sources (tried in order; every source serves identical bytes, so a partial download resumes against the next), size, sha256, unpacked_size, entry (executable, model file, or a marker file of a library pack, inside the version directory), license, requires (pack ids), optional hardware.

### Built-in packs: faster-whisper (BASE + optional NVIDIA GPU)

Local ASR is the system-managed provider `local.faster-whisper` (default model `large-v3-turbo`). Its runtime is split in two Windows x86_64 ZIP packs, built reproducibly (sorted entries, fixed timestamps, no bytecode caches) by `scripts/runtime/lock_faster_whisper.py --build <out>` from hash-locked requirement sets:

| Pack | Kind | Version | Archive bytes | Unpacked bytes | sha256 | Entry |
|---|---|---|---|---|---|---|
| `runtime.faster-whisper` (BASE, CPU) | runtime | 1.2.1-1 | 97,115,053 | 271,995,105 | `7663e0bf700d304221ca8936ac2f0e8e2af1cf0c0175db7d8d3e81d5847d513d` | `python.exe` |
| `runtime.faster-whisper.cuda` (optional NVIDIA GPU) | library | 1.2.1-1 | 1,372,642,304 | 2,108,417,417 | `2b7fd0ea278a93027b448aa976fde62c6aa92771232e8cdaaf294d4e7b4fa7a9` | `nvidia/cublas/bin/cublas64_12.dll` |
| `runtime.funasr` (SenseVoice, CPU) | runtime | 1.4.16-1 | 315,586,952 | 1,048,408,693 | `c126079b1ee52fe822de303277674cf4d91864413fab2d3d0ec40ed6e28ccf82` | `python.exe` |

- BASE (`workers/asr/requirements-faster-whisper-windows.lock`, 24 wheels, 85,380,903 bytes): embeddable CPython 3.12.0 (sha256 `c87f000e3dae1a572e98e81daeb622f8bc6f22664093fc9c70989b5f0018d49b`), faster-whisper 1.2.1, CTranslate2 4.8.3, numpy 2.5.4, tokenizers, huggingface_hub, plus `av` 19.0.2 (faster_whisper imports it at module load, although the worker reads the app-decoded 16 kHz mono WAV with `wave`) and `onnxruntime` 1.31.0 (Silero VAD for `vad_filter`). No venv and no system Python. Probe: the packaged `python.exe` imports `ctranslate2` and `faster_whisper` (120 s timeout).
- GPU (`workers/asr/requirements-faster-whisper-cuda-windows.lock`, 3 wheels, 1,372,639,935 bytes): nvidia-cublas-cu12 12.9.2.10, nvidia-cudnn-cu12 9.27.0.42, nvidia-cuda-nvrtc-cu12 12.9.86 (cuBLASLt and cuDNN load NVRTC). Requires BASE, hardware tag `nvidia-cuda`, presence-only probe. The install plan adds it only on Windows hosts with the NVIDIA driver (`%SystemRoot%\System32
- FunASR (`workers/asr/requirements-funasr-windows.lock`, 84 pins, about 301.8 MB of wheels, generated by `scripts/runtime/lock_funasr.py` with uv): embeddable CPython 3.12.0, funasr 1.4.16, torch 2.14.1+cpu, torchaudio 2.11.0+cpu. Four dependencies ship only as sdists and are built into wheels at pack time: antlr4-python3-runtime 4.9.3, crcmod 1.7, jieba 0.42.1, oss2 2.19.1. Serves `local.sensevoice` (default model `iic/SenseVoiceSmall`, fetched from ModelScope on first use). CPU only. Probe: imports funasr and torch (timeout 240 s, torch import is slow on first run). Release tag `runtime-funasr-1.4.16-1`, asset `runtime.funasr-1.4.16-1-windows-x86_64.zip` (not yet published; use `YUZUKI_RUNTIME_MIRROR` with a directory holding the zip). Verified 2026-10-27: mirror install 95 s (unpack, probe, activate), then a 14 s Japanese clip transcribed in about 65 s.
vcuda.dll`); everywhere else BASE alone is planned and CPU works.
- Device selection (worker request `device` or `YUZUKI_ASR_DEVICE`): `auto` uses CUDA + `int8_float16` only when the GPU pack is active and CTranslate2 sees a CUDA device, otherwise CPU + `int8`, and the reason is written to the result notes; `cuda` fails with a clear error when the GPU pack or device is missing and never switches to CPU; `cpu` forces CPU. The Desktop passes the active GPU pack version directory as `YUZUKI_FW_CUDA_DIR`; the worker registers its `nvidia/<lib>/bin` directories with `os.add_dll_directory` before importing CTranslate2.
- Build-host probe (2026-10-24): BASE and BASE + GPU both imported faster-whisper 1.2.1 / CTranslate2 4.8.3; with the GPU pack, 3 NVIDIA DLL directories were registered.
- Model checkpoints are not catalog packs yet: faster-whisper downloads the CTranslate2 checkpoint for the selected model through huggingface_hub on first use.

NOT YET PUBLISHED: both packs name, as their only source, an asset of the GitHub release tagged `runtime-faster-whisper-1.2.1-1` in the `avabbbb/Yuzuki` repository (asset name = the archive file name, see `fw_release_sources` in `runtime.rs`). That release and its two assets do not exist yet; until they are uploaded, a fresh install cannot download either pack (the developer override below still works).

### Retired packs

ADR-019 retired the previous native CLI runtime and its two model packs. On startup the Desktop deletes exactly those pack directories (`runtime::RETIRED_PACK_IDS`) and their active.json entries, logs what it removed, and touches nothing else under the runtime root. Persisted ASR requests that still name the retired provider (retried jobs, CLI/MCP calls) run on `local.faster-whisper` + `large-v3-turbo`, and the result notes say so.

### Transaction

download -> size + sha256 -> traversal-safe unpack -> probe -> rename into packs/ -> rewrite active.json (tmp + rename). Any failure before the active.json rename leaves the previous active version intact. Retryable failures keep the .part so the next attempt resumes. Installs run as runtime.install jobs (media_id 0) with stages download, verify, unpack, probe, activate, and support cancel and retry through the Jobs API.

### Commands

- runtime_install_plan(provider_id, model_id?): items, missing ids (dependency-first), download/required bytes, free bytes, fits, ready, blocker.
- install_runtime_packs(provider_id, model_id?): submits the install job; refuses when the plan has a blocker.
- runtime_doctor(): per pack state (active, broken, partial, missing, unsupported), active/previous version, update availability, staged bytes, probe report, developer override.

### Developer overrides

`TSUBAME_FASTER_WHISPER_PYTHON` (path to a Python with faster-whisper) wins over the active BASE pack. It is always reported (doctor output, ASR result notes) and is never a silent fallback; system Python is never used without it.

`TSUBAME_FUNASR_PYTHON` does the same for `runtime.funasr`: it wins over the active pack, is always reported, and `local.sensevoice` is ready only when either resolves.
