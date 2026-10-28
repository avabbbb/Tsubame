---
name: yuzuki
description: Bootstrap, diagnose, configure, launch, and operate the local YUZUKI Desktop. The yuzuki CLI and the yuzuki-mcp MCP server (both clients of the Desktop's local control endpoint) are implemented for doctor, app status, media import/list, ASR, segment, job and runtime operations.
---

# YUZUKI Skill

Use this Skill when the user wants to install, initialize, configure, launch, diagnose or operate YUZUKI.

## Implementation status (read first)

- Local control endpoint: implemented. The running Desktop listens on loopback only and writes control.json (address and bearer token) to its app data directory, readable only by the current user.
- yuzuki CLI: implemented for the commands marked implemented in references/commands.md. It is not yet bundled with the Desktop installers, so use a yuzuki binary on PATH or one built from crates/yuzuki-cli; if none exists, say so and fall back to the GUI.
- YUZUKI MCP server: implemented as yuzuki-mcp (stdio). Its tools are the registered operations, named with underscores (segment.update -> segment_update); see docs/07-agent-control.md. Like the CLI it is not yet bundled with the installers, so use a binary on PATH or one built from crates/yuzuki-mcp. An isError result carries the operation's error.code; branch on it, never on message text.
- Every command marked planned in references/commands.md is a contract target. Do not invoke it.

Always:
- pass --json and branch on the exit code and error.code, never on message text;
- treat exit 3 (desktop_not_running / launch_failed) as "the Desktop is not running"; pass --launch only when the user agreed to start it;
- never read, print or copy the token from control.json; the CLI handles it;
- never write to the YUZUKI SQLite database or asset store directly.

## Principle

YUZUKI Desktop is the product and canonical state owner.

The Skill orchestrates existing product interfaces. It must not duplicate provider logic, runtime installation logic, database writes or source parsing. The CLI itself carries no business logic: every command is one Operation Registry call on the Desktop.

Prefer, in order:

1. YUZUKI CLI (implemented commands only)
2. YUZUKI MCP (yuzuki-mcp), when the agent host speaks MCP
3. direct filesystem inspection only for read-only diagnostics
4. GUI interaction, for everything the CLI does not cover yet

## Startup flow

### 1. Locate installation

Check standard YUZUKI install locations and whether a yuzuki binary is available.

Then:

~~~bash
yuzuki app status --json
yuzuki doctor --json
~~~

Exit 3 from app status means the Desktop is not running. Start it through the GUI or, with the user's agreement, rerun a command with --launch.

### 2. Bootstrap when absent

Use the canonical YUZUKI release/bootstrap manifest.

Do not ask the user to manually install Python, Node, Rust, FFmpeg or CUDA toolchains.

Bootstrap should:
- resolve OS/arch;
- fetch a trusted Desktop release/bootstrap helper;
- verify integrity;
- install/launch YUZUKI;
- then return to CLI-driven initialization.

### 3. Initialize

Planned (PR #13), do not run: yuzuki init --json.

Today, yuzuki doctor --json reports the Runtime Pack state. Detect, where the Desktop exposes it:
- OS/arch;
- CPU/RAM;
- supported GPU/runtime acceleration;
- available native providers;
- existing runtime/model packs;
- configured remote providers;
- storage availability.

### 4. Resolve processing profile

Planned (PR #13), do not run: yuzuki profile list --json, yuzuki profile resolve "<profile>" --json.

If no profile is chosen, prefer a safe/default profile appropriate to the platform but never silently change a previously explicit user selection.

### 5. Plan runtime/model downloads

Planning and installing are keyed by provider and model today:

~~~bash
yuzuki runtime plan --provider-id <provider> --model-id <model> --json
~~~

Present material downloads/cost before execution when appropriate. Then, once the user agrees:

~~~bash
yuzuki runtime install --provider-id <provider> --model-id <model> --json
~~~

This queues an install job; follow it with yuzuki job list --json and stop it with yuzuki job cancel --id <job-id> --json.

### 6. Ensure Desktop

Planned, do not run: yuzuki app ensure. Use yuzuki app status --json, and --launch on a command when the user agreed to start the Desktop.

## Common operations

See references/commands.md for every command, its status and the operation it calls. yuzuki ops list --json returns the registered operations with their input schemas; yuzuki call <operation> --input '<json>' --json calls any of them.

## Dubbing run without pipeline ops (development path, measured 2026-10)

The Operation Registry exposes media.import_folder, media.list and asr.run (it queues the asr job; follow it with job list), so import and transcription go through the CLI/MCP. translate/tts/spatial/mix are not exposed yet (docs/issues/2026-10-dub-run.md I-03); until they are, a developer run drives those workers directly. Never use this path to write the Desktop database.

1. Decode: ffmpeg to 16 kHz mono PCM16 WAV (ASR input) and stereo WAV at source rate (spatial reference).
2. ASR: pipe one JSON request (`model_id`, `language`, `device`, `media_path`) to workers/asr/faster_whisper_worker.py on stdin; set YUZUKI_ASR_MODEL_DIR. Measured on RTX 3050 Ti 4 GB, 18:41 audio, large-v3-turbo int8_float16: about 58 s, VRAM peak about 3.2 GB. Use device=cuda explicitly; it fails instead of silently falling back to cpu.
3. Spatial analyze: send one protocol v1 `audio.spatialize` request with `input.op: analyze` to `python -m spatial.worker` from workers/dubber; stdin EOF drains and exits. Measured about 30 s for the same file.
4. Translate: only through a configured Desktop provider. A gateway answering 401 means no key is configured; ask the user to set it in Provider settings and never borrow another app's credential.
5. TTS: workers/tts/edge_tts_shim.py serves OpenAI `/v1/audio/speech` on loopback and needs HTTPS_PROXY for edge-tts. Requires edge-tts 7.3.1 or newer (older versions get handshake 403).
6. Spatial render per sentence (`input.op: render`), then mix.

All text sent to a remote LLM or TTS passes the redactor first (app/src-tauri/src/redact.rs; workers/tts/outbound_redact.py for the shim). Never put user directories, keys or tokens into logs, docs or screenshots.

### Measured notes (2026-10-25/26, two full runs, ~18.7 min ASMR source)

- ASR: faster_whisper_worker.py reads one JSON on stdin: contract_version 1, media_path (16 kHz mono WAV), model_id large-v3-turbo, language ja, device cuda. Run 1: 260 lines, 57.5 s, VRAM peak 3218 MiB. Run 2: 264 lines, 338.4 s, peak 3389 MiB (slowdown unexplained, see I-12; try HF_HUB_OFFLINE=1).
- Translate: gateway OpenAI chat, batches of 20, every ja line through for_cloud, budget 4.5 zh chars per second of slot. 260/260 in 342.8 s and 264/264 in 290.0 s, 0 retries, 0 over budget.
- Spatial analyze: analyze_file(source_stereo.wav), about 27-30 s. SpatialTrack.save(path) writes exactly that path, so pass spatial/spatial_track.npz (a bare name works for load but breaks globbing scripts).
- TTS: edge-tts 7.3.1, voice zh-CN-XiaoyiNeural, 2 concurrent requests, up to 5 retries with backoff (sporadic 502 from proxy connect timeouts, I-11). If a clip exceeds its slot by more than 5 %, re-synthesise once with speed = min(1.5, speed x dur/slot + 0.05). About 630-680 s for 260 lines; about half get sped up.
- The shim must patch WAV sizes (fix_wav_sizes): ffmpeg on a pipe leaves RIFF/data at 0xFFFFFFFF and every duration reads as about 89478 s (I-09).
- Before starting the shim, kill any listener on its port with taskkill /F /T: on Windows terminate() only ends the launcher and the old child keeps serving stale code (I-10). Confirm the stdout line "edge-tts shim listening ... proxy=set".
- Trim leading/trailing silence before placing clips (edge-tts adds about 0.17 s lead and 0.96 s tail); after trimming 259/260 and 261/264 fit their slots.
- Render + mix: workers/dubber/dev_render_mix.py RUN_DIR REPO source_trajectory. Clips resampled to 44.1 kHz, rendered at their start_ms, original ducked 9 dB under the dub, dub level matched to the original RMS over dub-active samples, sample peak ceiling 0.85. About 225 s including AAC mux. Result: about -25 LUFS, true peak -1.0 to -1.1 dBTP, per-line ILD correlation with the original 0.973, no renderer fallbacks.
- Export: mp4 with video copied, track 1 = zh dub over ducked ja (default, language chi), track 2 = untouched ja original (jpn).
- Human listening has not been done for these runs; only loudness and ILD checks.

## Mutation rules

For edits to existing objects, pass --expected-revision when the operation supports it (segment update, segment regenerate).

If a revision conflict occurs (exit 5, error.code revision_conflict):
1. re-read the object (yuzuki segment list --media-id <id> --json);
2. explain/reconcile the change;
3. retry only with a fresh expected revision.

## Provider secrets

Never print or request raw API keys if the Desktop can open its secure provider setup flow.

CLI output exposes redacted provider state and secret references only.

## Rights

Do not configure or generate an unauthorized real-person voice model. YUZUKI's name is a homage to voice actress Yuzuki Tsubame (柚木つばめ) and does not authorize use of her voice.
