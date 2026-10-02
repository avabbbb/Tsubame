# Yuzuki

> Local-first ASMR Library & AI Dubbing Workbench.

Yuzuki is a desktop workbench for discovering, downloading, organizing, playing, transcribing, translating, editing, dubbing, and exporting ASMR / voice media.

The product combines three ideas into one coherent desktop app:

- **ASPlayer-style playback and sentence timeline**
- **ASMR-Dubber-grade processing pipelines**
- **pluggable source/download adapters**

The result is not a downloader, player, and dubbing tool glued together. Yuzuki is one local-first workbench with a single library, one canonical segment timeline, one job system, and modular providers.

## Status

**Architecture-first baseline.**

We intentionally freeze product, data, provider, runtime, Skill, agent-control, and UI contracts before importing upstream implementation code.

## Product principles

1. **Desktop first.** End users install an app, not Python/Rust/Node toolchains.
2. **Workbench first.** The canonical shell is Activity Rail + Context Sidebar + Tabs + Main Workspace + Inspector + Bottom Bar.
3. **Segment first.** One sentence is an editable first-class object.
4. **Capability first.** Features depend on capabilities, never a hard-coded model or provider.
5. **BYOK by default for cloud AI.** Provider credentials are user-owned; supported providers can discover accessible models automatically.
6. **Local runtimes are modular.** Faster-Whisper, SenseVoice, Apple Speech, future ASR/TTS engines, etc. are replaceable providers.
7. **On-demand install.** Heavy runtimes/models are downloaded only when the chosen processing profile needs them.
8. **Agent-operable.** Local agents use the same product through the Yuzuki Skill + CLI/MCP/local RPC, not brittle GUI automation.
9. **Source adapters are isolated.** ASMR.one, Japanese ASMR, local folders, and future sources share one contract.
10. **No silent fallback.** Model/provider/runtime changes are explicit and auditable.

## Canonical workbench

```text
┌────┬──────────────┬──────────────────────────────────────┬──────────────────┐
│    │              │ Tabs                                 │                  │
│Rail│ Context      ├──────────────────────────────────────┤ Inspector        │
│    │ Sidebar      │                                      │                  │
│    │              │          Main Workspace              │                  │
│    │              │                                      │                  │
├────┴──────────────┴──────────────────────────────────────┴──────────────────┤
│ Global Player / Job Status Bar                                               │
└───────────────────────────────────────────────────────────────────────────────┘
```

## Processing model

```text
Feature
  ↓
Capability
  ↓
Processing Profile
  ↓
Provider
  ↓
Model
  ↓
Runtime / Transport
```

Examples:

- ASR → Faster-Whisper / Apple Speech / SenseVoice / Qwen ASR / future engines
- Transcript refinement → local or BYOK LLM
- Translation → OpenAI-compatible / Gemini / Alibaba Model Studio / local runtime
- TTS / voice synthesis → ASMR-Dubber adapters, local runtimes, remote APIs

## Agent + Skill model

The Desktop remains the product and canonical state owner.

```text
Local Agent
    │
  $yuzuki Skill
    │
 ┌──┴──────────────┐
 │                 │
CLI               MCP
 │                 │
 └──────┬──────────┘
        │
   Local RPC
        │
 Yuzuki Desktop
        │
 Operation Registry
```

The Skill is for fast initialization and orchestration:

- detect/install Yuzuki Desktop;
- run doctor checks;
- inspect hardware;
- resolve a processing profile;
- plan and download only required runtimes/models;
- launch or connect to the Desktop;
- operate the library, jobs, segments, providers and sources through stable interfaces.

## Planned documentation

- `docs/00-vision.md`
- `docs/01-product.md`
- `docs/02-architecture.md`
- `docs/03-ui-system.md`
- `docs/04-domain-model.md`
- `docs/05-processing-providers.md`
- `docs/06-runtime-bootstrap.md`
- `docs/07-agent-control.md`
- `docs/08-source-adapters.md`
- `docs/09-security-rights.md`
- `docs/10-roadmap.md`
- `docs/DECISIONS.md`
- `docs/UPSTREAM.md`
- `.agents/skills/yuzuki/SKILL.md`

## Upstream lineage

Implementation will selectively build on ideas/code from:

- [yumili426/ASPlayer](https://github.com/yumili426/ASPlayer) — GPL-3.0; Tauri/Vue/Rust player, SQLite library, sentence subtitles and local transcription.
- [EveningStudy/asmr-dubber](https://github.com/EveningStudy/asmr-dubber) — MIT; recoverable ASR/translation/alignment/TTS/mix pipeline.
- [fireinrain/asmr-downloader](https://github.com/fireinrain/asmr-downloader) — MIT; download/source architecture reference.

If ASPlayer code is incorporated, the distributed derivative remains GPL-3.0 and preserves required notices.

## Name

**Yuzuki** is a personal homage to voice actress **Tsubame Yuzuki (柚木つばめ)**.

This project is **not affiliated with, endorsed by, or an official product of Tsubame Yuzuki**. It does not bundle her recordings, voice model, likeness, branding assets, or training data.

The project must not be used to train, clone, or synthesize a real person's voice without the rights/permission required for that use.

## License

Planned: **GPL-3.0**, matching the ASPlayer lineage once ASPlayer code is incorporated.
