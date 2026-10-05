# Pull Request Plan

This document tracks the review-sized implementation sequence for Tsubame.

## PR #4 — React + Tauri/Rust + MIT clean-room foundation

**Status:** complete.

Goal: freeze the implementation stack and license before deeper product work.

Scope:
- Vue → React + TypeScript;
- keep Tauri 2 + Rust;
- keep SQLite canonical state;
- MIT license;
- remove ASPlayer implementation-lineage language;
- ASPlayer becomes reference-only;
- replace GPL/upstream docs with explicit dependency/reference boundaries;
- keep the already implemented Workbench/playback behavior.

Acceptance:
- frontend production build passes;
- Rust formatting/checks pass;
- docs check passes;
- no Vue source/dependency remains;
- repository license and Rust crate metadata are MIT;
- no current source imports or links GPL ASPlayer code;
- Workbench still builds with the same product shell.

Explicitly deferred:
- Segment Editor expansion;
- waveform;
- subtitle import/export;
- Provider Registry;
- production ASR;
- runtime/model installer.

---

## PR #5 — Canonical Segment Editor

**Status:** complete.

Goal: turn the sentence timeline into the primary editable data model.

Scope:
- stable Segment IDs/revisions;
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

## PR #6 — Provider Registry, BYOK and model discovery

**Status:** complete.

Goal: remove model/vendor assumptions from the product.

Scope:
- ProviderConfig, ModelDescriptor and capability vocabulary;
- Provider Registry + Capability Resolver;
- secure OS credential storage with secret_ref;
- provider connection test;
- model discovery + refresh;
- built-in catalog + discovered metadata merge;
- manual capability override for unknown models;
- generic OpenAI-compatible provider.

Acceptance:
- changing provider requires no feature/UI rewrite;
- raw keys never enter SQLite/logs;
- no feature switches on a vendor/model-name string.

---

## PR #7 — Modular ASR engines

Goal: make transcription a replaceable speech.asr capability.

Initial targets:
- Faster-Whisper;
- whisper.cpp as an independent optional engine;
- SenseVoice/FunASR;
- Apple Speech on supported macOS;
- cloud ASR adapter path.

Acceptance:
- same Track can run through different ASR providers;
- no ASR engine writes canonical SQLite directly;
- Japanese-language smoke fixtures exist.

---

## PR #8 — Transcript refinement and alignment

Goal: support ASMR-specific ASR → refine → align → human review.

Scope:
- transcript.refine capability;
- glossary/proper-noun context;
- conservative LLM correction proposals;
- diff/review;
- forced-alignment contract;
- provenance/confidence in Inspector.

---

## PR #9 — ASMR-Dubber worker integration

Goal: reuse compatible MIT processing capabilities without adopting its product UI.

Scope:
- versioned worker protocol;
- capability handshake;
- VAD/review/alignment/translation/TTS/timing/mix adapters;
- progress/cancel/retry;
- immutable worker inputs;
- per-sentence output asset contract.

---

## PR #10 — Single-sentence regenerate loop

Goal: prove Tsubame's defining editing experience.

```text
select Segment
 → edit
 → Ctrl/Cmd+Enter
 → resolve profile
 → synthesize only dirty sentence
 → update generated asset
 → preview immediately
```

---

## PR #11 — Runtime and model bootstrap

Goal: normal users configure no developer environment.

Scope:
- runtime/model manifests;
- hardware doctor;
- install planner;
- resumable downloads;
- integrity verification;
- staging + atomic activation;
- rollback;
- disk-space reporting.

---

## PR #12 — $tsubame Skill, CLI and MCP

Goal: let local agents operate the same Desktop without GUI automation.

Scope:
- doctor/app/provider/model/profile/runtime/library/segment/job commands;
- revision-checked mutations;
- localhost-only control channel;
- MCP mapped onto the same Operation Registry.

---

## PR #13 — Source Registry and shared Download Manager

Scope:
- SourceAdapter;
- Local adapter;
- persisted queue;
- concurrency/retry/backoff;
- proxy;
- range/resume;
- duplicate handling;
- staged finalization.

---

## PR #14 — ASMR.one compatibility adapter

Compatibility only; failure must not affect other sources.

---

## PR #15 — Japanese ASMR adapter

Canonical target: https://japaneseasmr.com/

Scope:
- discovery;
- work metadata;
- RJ/circle/CV normalization;
- asset probing;
- accessible media/subtitle acquisition;
- Japanese canonical names + romanized aliases.

---

## PR #16 — Library metadata and Apple Music media surfaces

Scope:
- Work/Track hierarchy;
- cover artwork;
- CV/circle/RJ metadata;
- continue listening;
- favorites/collections;
- artwork-led Work surfaces;
- optional cover-derived accents.

---

## PR #17 — Packaging, updater and first distributable release

Exit criteria: a non-developer can install Tsubame, acquire/import media, choose/install an ASR profile, transcribe, edit, translate, synthesize one sentence and continue playback without configuring a development toolchain.

## Parallelization

After PR #5 stabilizes Segment contracts:

```text
Provider Registry #6 ─┬─ ASR #7 ─ Refine/Align #8
                      └─ Runtime Bootstrap #11

ASMR-Dubber #9 ─ Single Segment #10

Source Registry #13 ─┬─ ASMR.one #14
                     └─ Japanese ASMR #15

Segment Editor #5 ─ Library polish #16
```
