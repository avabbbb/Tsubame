# Architecture

## Top-level shape

```text
Vue / TypeScript Workbench
          │
        Tauri 2
          │
┌─────────┼──────────────────────────────────────────┐
│         │                                          │
Domain    Library / Jobs                        Control Plane
SQLite    Runtime Manager                       CLI + MCP + local RPC
│         │                                          │
│    Capability Registry                            │
│         │                                          │
│   Provider / Model Registry                       │
│         │                                          │
├─────────┼───────────────┬──────────────────────────┤
│         │               │                          │
Sources   Native local    Worker packs               Remote BYOK
Adapters  providers       ASMR-Dubber adapter        providers
          Apple Speech    Faster-Whisper             OpenAI-compatible
                          SenseVoice                  Alibaba / Gemini / etc.
```

## Planned repository structure

```text
Yuzuki/
├─ app/                         # Tauri 2 + Vue workbench
├─ crates/
│  ├─ yuzuki-domain/
│  ├─ yuzuki-library/
│  ├─ yuzuki-jobs/
│  ├─ yuzuki-providers/
│  ├─ yuzuki-sources/
│  ├─ yuzuki-runtime/
│  └─ yuzuki-control/
├─ workers/
│  ├─ dubber/
│  └─ provider-packs/
├─ schemas/
├─ examples/
├─ .agents/skills/yuzuki/
├─ docs/
└─ scripts/
```

## State ownership

**Yuzuki SQLite is the canonical product state.**

Worker project files, caches and manifests may exist for execution/recovery but never become the source of truth for user-edited segments.

## Operation layer

UI, CLI and MCP must converge on an operation registry rather than implementing separate business logic.

Example operations:

- `library.import`
- `source.search`
- `source.acquire`
- `segment.update`
- `segment.regenerate`
- `processing.run`
- `runtime.install`
- `provider.refresh_models`
- `job.cancel`

Each operation declares:
- typed input/output;
- read/mutation status;
- side effects;
- expected revision requirements;
- cost/download implications;
- progress events;
- cancellation semantics.

## Execution boundaries

Use native Rust when it improves app integration, I/O safety or packaging.

Use isolated workers when an AI ecosystem is materially better outside Rust. Workers communicate through versioned contracts and cannot directly mutate canonical SQLite state.

## ASMR-Dubber role

ASMR-Dubber is treated as an execution-engine lineage, not a second product UI.

Yuzuki should reuse/adapt its strongest ideas:
- VAD/segmentation;
- ASR backends;
- review/alignment;
- translation batching/memory;
- per-sentence TTS caching;
- timing/mixing;
- recoverable runtime/model management.

Its Gradio UI is not the user-facing Yuzuki product.

## ASPlayer role

ASPlayer is the desktop/player lineage:
- Tauri/Vue/Rust shell;
- playback;
- SQLite media state;
- sentence subtitle timeline;
- local transcription flow;
- dictionary/learning utilities where still relevant.

Yuzuki restructures it around the Workbench shell and modular processing architecture.
