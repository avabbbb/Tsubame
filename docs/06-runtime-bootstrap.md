# Runtime Bootstrap and Desktop Packaging

## User promise

A normal user installs Yuzuki Desktop and does not manually configure developer environments.

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
Yuzuki installer
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
YuzukiData/
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

Updating Yuzuki must not force re-downloading multi-GB models when compatible packs already exist.

## Skill bootstrap

The Yuzuki Skill may:
- detect an existing installation;
- download a trusted Desktop release/bootstrap helper when absent;
- run `doctor`;
- ask the Desktop/runtime manager to install missing packs.

The Skill must not reproduce environment-management logic independently. Runtime installation logic belongs to Yuzuki.
