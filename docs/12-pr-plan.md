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

## PR #7 — Modular ASR adapters

**Status:** complete.

Goal: make transcription execution replaceable behind one canonical ASR result contract.

Initial adapters:
- Faster-Whisper;
- whisper.cpp;
- SenseVoice/FunASR;
- Apple Speech;
- OpenAI-compatible transcription.

Acceptance:
- no ASR adapter writes canonical SQLite directly;
- Japanese-language smoke fixtures exist;
- invalid/failed ASR never deletes the current transcript.

---

## PR #8 — Unified local/remote Provider execution

**Status:** complete.

Goal: remove the accidental second “ASR Engine Registry” and make local + remote execution first-class Providers.

Scope:
- Provider execution modes: remote_api / local_server / local_runtime / native_os;
- system-managed local Providers;
- Provider + Model selection in Studio;
- ASR adapters become internal Provider implementation details;
- local Provider models resolve through the same Capability Resolver;
- LM Studio/local OpenAI-compatible preset;
- provenance records Provider + Model;
- translation/refinement architecture explicitly supports both paid APIs and local LLMs.

Acceptance:
- Faster-Whisper, SenseVoice, whisper.cpp and Apple Speech appear as Providers;
- Studio selects Provider + Model, never Engine + Model;
- local server models and remote API models can satisfy the same capability;
- system-managed local Providers cannot be deleted/overwritten;
- no feature needs an “online vs local” branch.

---

## PR #9 — Transcript refinement and alignment

**Status:** complete. See [Transcript Refinement and Alignment](18-transcript-refinement.md).

Goal: support ASMR-specific ASR → refine → align → human review.

Scope:
- transcript.refine capability;
- local or remote LLM Provider;
- glossary/proper-noun context;
- conservative correction proposals;
- diff/review;
- forced-alignment contract;
- provenance/confidence in Inspector.

---

## PR #10 — ASMR-Dubber worker integration

Goal: reuse compatible MIT processing capabilities without adopting its product UI.

Scope:
- versioned worker protocol;
- capability handshake;
- VAD/review/alignment/translation/TTS/timing/mix adapters;
- progress/cancel/retry;
- immutable worker inputs;
- per-sentence output asset contract.

---

## PR #11 — Single-sentence regenerate loop

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

## PR #12 — Runtime and model bootstrap

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

## PR #13 — $tsubame Skill, CLI and MCP

Goal: let local agents operate the same Desktop without GUI automation.

Scope:
- doctor/app/provider/model/profile/runtime/library/segment/job commands;
- revision-checked mutations;
- localhost-only control channel;
- MCP mapped onto the same Operation Registry.

---

## PR #14 — Source Registry and shared Download Manager

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

## PR #15 — ASMR.one compatibility adapter

Compatibility only; failure must not affect other sources.

---

## PR #16 — Japanese ASMR adapter

Canonical target: https://japaneseasmr.com/

Scope:
- discovery;
- work metadata;
- RJ/circle/CV normalization;
- asset probing;
- accessible media/subtitle acquisition;
- Japanese canonical names + romanized aliases.

---

## PR #17 — Library metadata and Apple Music media surfaces

Scope:
- Work/Track hierarchy;
- cover artwork;
- CV/circle/RJ metadata;
- continue listening;
- favorites/collections;
- artwork-led Work surfaces;
- optional cover-derived accents.

---

## PR #18 — Packaging, updater and first distributable release

Exit criteria: a non-developer can install Tsubame, acquire/import media, choose/install an ASR profile, transcribe, edit, translate, synthesize one sentence and continue playback without configuring a development toolchain.

## Parallelization

After PR #8 unifies Provider execution:

```text
Unified Providers #8 ─┬─ Refine/Align #9
                      └─ Runtime Bootstrap #12

ASMR-Dubber #10 ─ Single Segment #11

Source Registry #14 ─┬─ ASMR.one #15
                     └─ Japanese ASMR #16

Segment Editor #5 ─ Library polish #17
```
