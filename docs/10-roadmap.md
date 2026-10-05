# Roadmap

For the concrete review-sized sequence and acceptance criteria, see [Pull Request Plan](12-pr-plan.md).

## Completed foundation

### PR #1 — Product/architecture baseline

Defined the initial product, Workbench, provider/runtime, Skill, source and rights contracts.

### PR #2 — Runnable Desktop baseline

Established the first working desktop prototype with local media playback, SQLite-backed Segments and the Workbench shell.

### PR #3 — Persistent Workbench and playback parity

Added resizable/collapsible panes, persistent tabs, resume position, speed/volume persistence, segment navigation, looping, keyboard shortcuts, themes and a global player.

## Current reset

### PR #4 — React + Tauri/Rust + MIT clean-room foundation

Freeze the long-term implementation boundary:

- React + TypeScript frontend;
- Tauri 2 + Rust native shell;
- SQLite canonical state;
- MIT repository license;
- ASPlayer becomes product/interaction reference only;
- no GPL source is copied or linked.

## Next

### PR #5 — Canonical Segment Editor

- editable source/translation/start/end;
- SRT/VTT import/export;
- waveform/timeline;
- undo/redo;
- provenance;
- dirty dependency graph;
- revision-conflict UI.

### PR #6 — Provider/Model Registry

- ProviderConfig / ModelDescriptor;
- capability resolver;
- BYOK secrets;
- model discovery;
- generic OpenAI-compatible provider;
- local provider contract.

### PR #7 — Modular ASR

- Faster-Whisper;
- whisper.cpp as an independent optional engine;
- SenseVoice/FunASR;
- Apple Speech on macOS;
- cloud ASR adapters.

### PR #8 — Transcript refinement and alignment

- optional LLM refinement;
- glossary/proper nouns;
- conservative review proposals;
- forced alignment;
- provenance/confidence.

### PR #9 — ASMR-Dubber worker

Integrate compatible MIT processing capabilities behind versioned worker contracts.

### PR #10 — Single-sentence regenerate

Prove the defining loop:

```text
edit one Segment
 → resolve profile
 → regenerate only that Segment
 → preview immediately
```

### PR #11 — Runtime/model bootstrap

App-managed runtime/model packs with resumable verified installation.

### PR #12 — $tsubame Skill + CLI/MCP

Agent-operable control plane over the same Desktop operations.

### PR #13 — Source Registry + Download Manager

Shared source and acquisition infrastructure.

### PR #14 — ASMR.one compatibility adapter

Compatibility-only adapter; not a foundational dependency.

### PR #15 — Japanese ASMR adapter

Canonical target: https://japaneseasmr.com/

### PR #16 — Library metadata and Apple Music media polish

Work/Track/CV/Circle/RJ metadata, artwork-forward surfaces and collections.

### PR #17 — Packaging / updater / first distributable release

Windows x64 installer, updater, clean-machine validation and runtime bootstrap smoke tests.
