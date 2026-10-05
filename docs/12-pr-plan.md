# Pull Request Plan

This document turns the architecture roadmap into reviewable implementation slices. Each PR must leave `main` usable and must not smuggle the next layer into the current one.

## PR #2 — ASPlayer → Tsubame desktop baseline

**Status:** in progress.

Scope:
- Tauri 2 + Vue/TypeScript application;
- SQLite local media library;
- local audio/video import and playback;
- canonical `Segment` table with optimistic `revision`;
- click-to-seek timeline;
- Inspector edit/save path;
- persistent player bar;
- Codex/VS Code workbench geometry with Apple Music visual language;
- record the exact ASPlayer upstream baseline and GPL lineage.

Acceptance:
- frontend production build passes;
- Rust formatting/checks pass;
- docs check passes;
- imported local media plays;
- the app can read/edit persisted Segments without provider-specific code;
- no Whisper/model-specific dependency is introduced into the product shell.

Explicitly deferred:
- production ASR;
- remote providers;
- runtime/model installer;
- source-site downloading;
- ASMR-Dubber worker.

---

## PR #3 — Workbench shell and ASPlayer playback parity

**Status:** in progress.

Goal: make Tsubame feel like a real desktop media app before adding AI complexity.

Scope:
- resizable/collapsible Context Sidebar and Inspector;
- persistent tabs for opened Works/Tracks/search/jobs;
- restore useful ASPlayer playback behavior: resume position, speed, volume, previous/next sentence, sentence loop and AB loop where compatible;
- audio/video stage behavior;
- keyboard shortcuts;
- global bottom player;
- cover/work header;
- light/dark themes;
- Apple Music-inspired artwork/material treatment;
- cover-derived accent as an enhancement, never a readability dependency.

Acceptance:
- playback continues while switching product surfaces;
- panel state survives restart;
- no feature requires opening a modal “player page”;
- Main Workspace remains the priority surface on narrow windows.

---

## PR #4 — Canonical Segment Editor

Goal: turn the subtitle timeline into the primary editable data model.

Scope:
- stable Segment IDs and revision migration;
- source text / translation / start / end editing;
- SRT/VTT import into Segments;
- SRT/VTT export from Segments;
- waveform/timeline context;
- current-sentence highlight and click-to-seek;
- undo/redo;
- dirty dependency graph;
- provenance slots for ASR/refine/translate/TTS;
- revision-conflict UI for human/agent concurrent edits.

Acceptance:
- editing translation does not invalidate ASR;
- editing timing does not force TTS regeneration;
- edits round-trip through restart/export;
- stale agent mutations fail cleanly.

---

## PR #5 — Provider Registry, BYOK and model discovery

Goal: remove model/vendor assumptions from the product.

Scope:
- `ProviderConfig`, `ModelDescriptor`, capability vocabulary;
- Provider Registry and Capability Resolver;
- secure OS credential storage with `secret_ref`;
- provider connection test;
- model discovery + refresh;
- built-in catalog + discovered metadata merge;
- manual capability override for unknown models;
- generic OpenAI-compatible provider;
- first remote translation/refinement providers.

Initial provider targets:
- OpenAI-compatible;
- Alibaba Model Studio;
- Gemini-compatible path where appropriate;
- local-provider registration contract.

Acceptance:
- changing translation provider requires no feature/UI rewrite;
- raw keys never enter SQLite/logs;
- model discovery failure does not destroy existing configuration;
- no product feature switches on a vendor/model-name string.

---

## PR #6 — Modular ASR engines

Goal: replace “transcription = Whisper” with `speech.asr`.

Targets:
- Faster-Whisper as the primary Windows local candidate;
- whisper.cpp retained as an optional lightweight/local engine from ASPlayer lineage;
- SenseVoice/FunASR;
- Apple Speech provider contract and macOS implementation when platform support is available;
- cloud ASR adapter path for providers such as Alibaba.

Shared output:
- canonical Segment candidates;
- timestamps;
- language;
- confidence/provenance where available.

Shared processing:
- media extraction;
- VAD/segmentation contract;
- cancellation/progress;
- resumability.

Acceptance:
- the same Track can be re-run with a different ASR provider;
- no ASR engine writes canonical SQLite directly;
- Japanese-language smoke fixtures exist.

---

## PR #7 — Transcript refinement, review and alignment

Goal: support the ASMR-specific `ASR → refine → align → human review` workflow.

Scope:
- optional `transcript.refine` capability;
- glossary/proper-noun context;
- conservative LLM correction mode;
- candidate diff/review;
- forced-alignment provider contract;
- provenance and confidence display in Inspector;
- “accept proposal / keep original” flow.

Acceptance:
- refiner cannot silently overwrite reviewed text;
- original ASR result remains inspectable;
- alignment can run independently from ASR.

---

## PR #8 — ASMR-Dubber worker integration

Goal: reuse the mature processing lineage without embedding its Gradio product UI.

Scope:
- versioned worker protocol;
- worker health/capability handshake;
- adapt useful VAD/review/alignment/translation/TTS/timing/mix pieces;
- job progress/cancel/retry events;
- immutable worker inputs;
- results returned to Tsubame Operation Registry;
- per-sentence generated audio asset contract.

Acceptance:
- worker never mutates canonical SQLite;
- worker crash leaves a recoverable Job;
- UI, CLI and future MCP observe the same job state.

---

## PR #9 — Single-sentence regenerate loop

Goal: prove Tsubame’s defining editing experience.

Flow:

```text
select Segment
 → edit translation / timing / voice setting
 → Ctrl/Cmd + Enter
 → resolve profile
 → synthesize only dirty sentence
 → update generated asset
 → local mix invalidation
 → preview immediately
```

Scope:
- Segment-level TTS cache key;
- original/generated A/B preview;
- per-segment regenerate;
- dirty-only processing;
- local partial re-mix strategy.

Acceptance:
- changing one translated sentence never re-runs full-track ASR/TTS;
- generated result is immediately playable from Inspector/timeline;
- failure preserves the previous good generated asset.

---

## PR #10 — Runtime and model bootstrap

Goal: make the Desktop installation self-contained for normal users.

Scope:
- Runtime Pack / Model Pack manifests;
- hardware/platform doctor;
- install planner;
- resumable downloads;
- SHA-256/integrity verification;
- staging + atomic activation;
- previous-known-good rollback;
- disk-space reporting;
- app-managed FFmpeg/media helpers;
- GitHub Release/bootstrap metadata.

Acceptance:
- fresh Windows user does not install Python/Node/Rust manually;
- selecting a Processing Profile produces a deterministic install plan;
- interrupted model/runtime downloads resume safely.

---

## PR #11 — `$tsubame` Skill, CLI and MCP control plane

Goal: let local agents operate the same Desktop without GUI automation.

Scope:
- `tsubame doctor --json`;
- `tsubame app ensure`;
- provider/model/profile/runtime commands;
- library/work/track/segment/job commands;
- revision-checked mutations;
- localhost-only authenticated control channel;
- MCP tools mapped onto the same Operation Registry;
- Skill bootstrap/initialization instructions.

Acceptance:
- local agent can launch/connect to Tsubame and edit/regenerate a Segment;
- CLI/UI/MCP share behavior and state;
- secrets are redacted from agent-visible responses.

---

## PR #12 — Source Registry and shared Download Manager

Goal: build acquisition infrastructure once before website-specific adapters multiply.

Scope:
- SourceAdapter contract;
- Local source adapter;
- persisted download queue;
- concurrency/retry/backoff;
- proxy support;
- range/resume where available;
- duplicate handling;
- staged/atomic finalization;
- source-to-library mapping.

Acceptance:
- source-specific code has no direct UI/database shortcuts;
- downloaded assets remain usable if the source later disappears.

---

## PR #13 — ASMR.one compatibility adapter

Goal: preserve useful compatibility with the older source without making it foundational.

Scope:
- search/detail/asset probing where the service still functions;
- metadata normalization;
- graceful degraded/unavailable state.

Acceptance:
- adapter failure cannot break Local/Japanese ASMR/library playback.

---

## PR #14 — Japanese ASMR adapter

Canonical target: https://japaneseasmr.com/

Scope:
- search/discovery;
- work metadata;
- RJ code and circle/CV normalization;
- track list/asset probing;
- accessible audio/video/subtitle acquisition;
- voice-actor canonical Japanese names plus romanized aliases;
- fixtures for markup/parser changes.

Acceptance:
- at least one real work can complete `discover → inspect → acquire → library → play`;
- no DRM/paywall/access-control bypass;
- adapter parser changes are isolated and tested.

---

## PR #15 — Library metadata and Apple Music media surfaces

Goal: make the library feel like a polished media product rather than a file list.

Scope:
- Work / Track hierarchy;
- cover artwork;
- CV / circle / RJ metadata;
- recently added / continue listening / favorites / collections;
- artwork-forward Work header;
- optional cover-derived accent/material;
- search and sorting;
- metadata merge rules across Local/source adapters.

Acceptance:
- filename/path is no longer the main library identity;
- missing artwork/metadata degrades cleanly;
- media aesthetics never obscure editing readability.

---

## PR #16 — Packaging, updater and first distributable release

Scope:
- Windows x64 installer;
- application updater;
- app/runtime/model update channels separated;
- release notes and license/notices;
- clean-machine smoke test;
- runtime/model bootstrap smoke test;
- crash/recovery and database migration checks.

Exit criteria:
- a non-developer can install Tsubame, import/acquire media, choose/install an ASR profile, transcribe, edit, translate, synthesize one sentence and continue playback without configuring a development environment.

---

## Parallelization rules

After PR #4 stabilizes Segment contracts, work may proceed in parallel:

```text
Provider Registry (#5) ─┬─ ASR (#6) ─ Refine/Align (#7)
                        └─ Runtime Bootstrap (#10)

ASMR-Dubber Worker (#8) ─ Single-sentence Regenerate (#9)

Source Registry (#12) ─┬─ ASMR.one (#13)
                       └─ Japanese ASMR (#14)

Workbench (#3) ─ Segment Editor (#4) ─ Library polish (#15)
```

Do not parallelize two PRs that both redefine the same persistent schema or provider contracts without a shared base PR first.
