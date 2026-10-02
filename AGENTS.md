# AGENTS.md — Yuzuki

This file is the execution contract for coding agents working in this repository.

## Product invariant

Yuzuki is a **local-first desktop ASMR library and AI dubbing workbench**.

It is not:
- a web app that happens to be wrapped by Tauri;
- a downloader with an AI tab;
- a hard-coded Whisper player;
- a UI wrapper around one provider;
- an agent-only product.

The Desktop remains fully usable by a normal user. Agents operate the same product through stable local interfaces.

## Read first

Before changing contracts or implementation, read:

1. `docs/01-product.md`
2. `docs/02-architecture.md`
3. `docs/03-ui-system.md`
4. `docs/04-domain-model.md`
5. `docs/05-processing-providers.md`
6. `docs/06-runtime-bootstrap.md`
7. `docs/07-agent-control.md`
8. `docs/09-security-rights.md`

## Non-negotiable rules

1. **Capability first.** Product features never depend directly on a concrete model/provider name.
2. **Provider != model.** Providers own authentication, discovery and transport. Models declare capabilities.
3. **Desktop is canonical.** Yuzuki SQLite owns product state. Worker caches/manifests are execution state.
4. **Segment is first-class.** One sentence can be edited, regenerated and invalidated independently.
5. **No environment setup for end users.** Do not require global Python, Node, Rust, FFmpeg or CUDA tooling.
6. **App-managed runtimes.** Heavy engines/models install into Yuzuki-owned directories through verified manifests.
7. **BYOK secrets belong in OS credential storage.** Never store API keys in SQLite, logs, project manifests or git.
8. **Agent operations use CLI/MCP/local RPC.** Never make GUI automation the canonical control path.
9. **Source-specific logic stays in adapters.** No website parser in UI/domain code.
10. **Workbench shell is canonical.** Features attach to Rail, Sidebar, Tabs, Main, Inspector or Bottom Bar.
11. **No silent fallback.** Provider/model/runtime changes must be visible and auditable.
12. **Real-person voice rights are explicit.** Never ship or train a real-person voice model without documented permission/rights.

## Change discipline

- Prefer small, reviewable PRs.
- Contract changes update the corresponding canonical document.
- Persistent schema changes require migration tests.
- Providers require capability conformance tests.
- Source adapters require adapter contract tests.
- Runtime downloads require hash/integrity verification tests.
- Do not commit model binaries, downloaded media, user assets, secrets, caches or generated runtimes.

## Definition of done

A feature is not done only because its UI renders.

It must define:
- state ownership;
- operation contract;
- failure semantics;
- cancellation/retry behavior where relevant;
- persistence/migration impact;
- agent/CLI surface when the feature is automatable;
- security/rights impact;
- tests.
