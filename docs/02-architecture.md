# Architecture

## Top-level shape

```text
React / TypeScript Workbench
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
YUZUKI/
├─ app/                         # Tauri 2 + React workbench
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

**YUZUKI SQLite is the canonical product state.**

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

YUZUKI should reuse/adapt its strongest ideas:
- VAD/segmentation;
- ASR backends;
- review/alignment;
- translation batching/memory;
- per-sentence TTS caching;
- timing/mixing;
- recoverable runtime/model management.

Its Gradio UI is not the user-facing YUZUKI product.

## ASPlayer boundary

ASPlayer is **not** an implementation lineage or code dependency.

It may be used only as a product/interaction reference for ideas such as:
- sentence-aware playback;
- timeline navigation;
- subtitle ergonomics;
- media-learning workflows.

YUZUKI implements these capabilities independently. No GPL-licensed ASPlayer source may be copied, adapted, vendored or linked into the MIT codebase.

## Desktop stack

The canonical Desktop stack is:

```text
React + TypeScript
       ↓
    Tauri 2
       ↓
      Rust
       ↓
SQLite / filesystem / jobs / runtime management
```

Rust owns desktop-local state and native boundaries. Python is reserved for isolated AI workers only when the Python ecosystem provides a material advantage.
