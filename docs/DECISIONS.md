# Architecture Decisions

## ADR-001 — Tsubame is a Workbench, not a set of pages

Accepted.

Canonical shell: Activity Rail, Context Sidebar, Tabs, Main Workspace, Inspector, Bottom Bar.

## ADR-002 — SQLite is canonical product state

Accepted.

Workers may keep execution caches/manifests but cannot own user-edited timeline truth.

## ADR-003 — Segment is first-class

Accepted.

Editing one sentence invalidates only downstream dependencies.

## ADR-004 — Capability → Provider → Model

Accepted.

Features cannot hard-code concrete provider/model names.

## ADR-005 — Provider and model are separate concepts

Accepted.

Provider = auth/discovery/transport. Model = capability descriptor.

## ADR-006 — Desktop users configure no developer environment

Accepted.

Runtime/model dependencies are app-managed and installed on demand.

## ADR-007 — CLI/MCP share the Desktop operation layer

Accepted.

Agents do not receive a separate business implementation.

## ADR-008 — Source sites are adapters

Accepted.

ASMR.one and Japanese ASMR are initial adapters, not foundational domain types.

## ADR-009 — ASPlayer is reference-only

Accepted.

ASPlayer may inform product/interaction research, but its GPL-3.0 source is not copied, adapted, vendored or linked into Tsubame. Tsubame is a clean-room MIT implementation.

ASMR-Dubber remains a potential MIT processing dependency behind worker contracts.

## ADR-010 — GalGame mode is not on the initial roadmap

Accepted.

It does not contribute to the core acquisition → transcript → edit → dub loop.

## ADR-011 — Real-person voice use requires rights/permission

Accepted.

The project name is a homage only and does not grant permission to use the namesake's voice/model/recordings.


## ADR-012 — Product name is Tsubame

Accepted.

The repository and product name are **Tsubame**, a personal homage to **Yuzuki Tsubame / 柚木つばめ**. The shorter name is more distinctive than “Yuzuki” while still pointing to the intended namesake. This naming decision does not imply affiliation or permission to use her voice; `docs/09-security-rights.md` remains authoritative for that boundary.


## ADR-013 — React + Tauri 2 + Rust is the canonical Desktop stack

Accepted.

Tsubame uses React/TypeScript for the Workbench UI and Tauri 2/Rust for the native Desktop shell, SQLite ownership, filesystem boundaries, local jobs and runtime management.

Rust is retained because it is a strong fit for the existing Desktop architecture. Python remains isolated to AI workers when justified by model ecosystem support.

## ADR-014 — Tsubame is MIT

Accepted.

MIT compatibility is protected by excluding GPL source code from the repository. Reference-only research does not authorize source reuse.


## ADR-015 — Provider, model and capability are separate

Accepted.

Provider owns authentication, transport and model discovery.

Model owns identity plus capability metadata.

Features resolve a capability and cannot hard-code a vendor/model ID.

Inference base URL and model-discovery URL are independent because compatible providers may expose them on different paths.

## ADR-016 — Raw provider secrets never enter SQLite

Accepted.

Provider rows store only a `secret_ref`. The Desktop resolves the actual credential through the OS credential store. UI, CLI and MCP surfaces expose redacted state/reference metadata only.
