# Roadmap

For the concrete review-sized sequence and acceptance criteria, see [Pull Request Plan](12-pr-plan.md).

The roadmap is ordered to prove architecture and user value, not to maximize feature count.

## PR 0 — Product/architecture baseline

This documentation set.

Freeze:
- product boundary;
- Workbench UI;
- canonical domain state;
- provider/model architecture;
- runtime/bootstrap contract;
- agent/Skill control plane;
- source adapter contract;
- rights/licensing boundaries.

## PR 1 — ASPlayer → Tsubame baseline migration

Bring in the useful desktop lineage while preserving attribution/license:
- Tauri 2 + Vue;
- playback;
- media import/library;
- SQLite;
- sentence subtitle timeline;
- local job/event foundations.

Do not preserve old information architecture just because it exists upstream.

## PR 2 — Workbench Shell

Implement:
- Activity Rail;
- Context Sidebar;
- Tabs;
- Main Workspace;
- Inspector;
- Bottom Player/Job Bar;
- resize/collapse state;
- compact Notion/Codex visual tokens.

## PR 3 — Canonical Segment Editor

Add:
- stable Segment IDs/revisions;
- source/translation/timing editing;
- waveform context;
- dirty dependency graph;
- undo/redo;
- sentence preview.

## PR 4 — Provider/Model Registry

Separate:
- Provider;
- ModelDescriptor;
- capability resolution;
- BYOK secret references;
- model discovery;
- local runtime discovery.

Keep whisper.cpp as one provider, not the architecture.

## PR 5 — ASMR-Dubber Worker Integration

Adapt useful processing capabilities behind worker contracts:
- segmentation/VAD;
- ASR families;
- refinement/review;
- alignment;
- translation;
- per-sentence TTS;
- mix/export.

## PR 6 — Single-sentence regenerate loop

Prove the key experience:

```text
select sentence
 → edit
 → Ctrl/Cmd+Enter
 → regenerate only affected sentence
 → preview/replay immediately
```

## PR 7 — Runtime Bootstrap

Windows x64:
- app-managed runtime directory;
- runtime/model manifests;
- resumable verified install;
- doctor;
- no global environment setup.

## PR 8 — Tsubame Skill + CLI/MCP

Implement:
- install detection/bootstrap;
- doctor;
- app ensure/connect;
- provider/model queries;
- runtime plan/install;
- library/segment/job operations.

## PR 9 — Source Registry + Local / ASMR.one

Build shared download manager and first adapters.

## PR 10 — Japanese ASMR Adapter

Add metadata/asset probing and lawful acquisition of accessible assets.

## PR 11 — Packaging/Release

Produce normal Windows installer + release artifacts, with heavy models/runtimes installed on demand.

## Later

- macOS + Apple Speech provider;
- more ASR/TTS providers;
- advanced batch workflows;
- optional overlay/dictionary/learning surfaces;
- richer export presets.
