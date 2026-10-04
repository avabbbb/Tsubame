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

## ADR-009 — ASPlayer is desktop lineage, ASMR-Dubber is processing lineage

Accepted.

Tsubame may incorporate/adapt code while replacing both projects' user-facing product boundaries with its canonical contracts.

## ADR-010 — GalGame mode is not on the initial roadmap

Accepted.

It does not contribute to the core acquisition → transcript → edit → dub loop.

## ADR-011 — Real-person voice use requires rights/permission

Accepted.

The project name is a homage only and does not grant permission to use the namesake's voice/model/recordings.


## ADR-012 — Product name is Tsubame

Accepted.

The repository and product name are **Tsubame**, a personal homage to **Yuzuki Tsubame / 柚木つばめ**. The shorter name is more distinctive than “Yuzuki” while still pointing to the intended namesake. This naming decision does not imply affiliation or permission to use her voice; `docs/09-security-rights.md` remains authoritative for that boundary.
