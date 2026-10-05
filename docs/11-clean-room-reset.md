# Clean-room Desktop Reset

PR #4 resets Tsubame's implementation boundary before the product grows further.

## Decision

Canonical Desktop stack:

```text
React + TypeScript
       ↓
    Tauri 2
       ↓
      Rust
       ↓
SQLite / filesystem / jobs / runtime management
```

License: **MIT**.

## Why this reset exists

Early prototype work used Vue terminology and described ASPlayer as an implementation lineage. That is no longer the product decision.

Tsubame only needs some of the **ideas** visible in ASPlayer:
- sentence-aware playback;
- subtitle/timeline workflows;
- looping/navigation ergonomics.

Those ideas are reimplemented independently.

## License boundary

ASPlayer is GPL-3.0 and is therefore reference-only for this MIT project.

This PR establishes that:
- no ASPlayer source is copied into the current implementation;
- no ASPlayer package/source tree is vendored;
- no GPL dependency is linked into the Desktop;
- future contributors must preserve this boundary unless the project explicitly changes license.

## What remains

The independently implemented product contracts remain valid:

- Workbench shell;
- global player;
- SQLite-owned media state;
- canonical Segment model;
- revision-checked edits;
- provider/capability architecture;
- Skill/CLI/MCP direction;
- source adapters.

## What changes

- Vue → React;
- GPL-3.0 repository license → MIT;
- “ASPlayer lineage” → “ASPlayer reference only”;
- implementation docs and CI now describe React + Tauri 2 + Rust.

## Python boundary

Python is not part of the Desktop shell.

Python workers may be packaged later for AI capabilities where ecosystems such as Faster-Whisper, FunASR or TTS frameworks make it advantageous. Those workers communicate through versioned contracts and do not own canonical SQLite state.
