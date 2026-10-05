---
name: tsubame
description: Bootstrap, diagnose, configure, launch, and operate the local Tsubame Desktop through its stable CLI/MCP control plane.
---

# Tsubame Skill

Use this Skill when the user wants to install, initialize, configure, launch, diagnose or operate Tsubame.

## Principle

Tsubame Desktop is the product and canonical state owner.

The Skill orchestrates existing product interfaces. It must not duplicate provider logic, runtime installation logic, database writes or source parsing.

Prefer, in order:

1. Tsubame CLI
2. Tsubame MCP
3. local RPC through a supported client
4. direct filesystem inspection only for diagnostics
5. GUI interaction only when no stable operation exists

## Startup flow

### 1. Locate installation

Check standard Tsubame install locations and whether `tsubame` CLI is available.

If installed:

```bash
tsubame app status --json
tsubame doctor --json
```

### 2. Bootstrap when absent

Use the canonical Tsubame release/bootstrap manifest.

Do not ask the user to manually install Python, Node, Rust, FFmpeg or CUDA toolchains.

Bootstrap should:
- resolve OS/arch;
- fetch a trusted Desktop release/bootstrap helper;
- verify integrity;
- install/launch Tsubame;
- then return to CLI-driven initialization.

### 3. Initialize

```bash
tsubame init --json
tsubame doctor --json
```

Detect:
- OS/arch;
- CPU/RAM;
- supported GPU/runtime acceleration;
- available native providers;
- existing runtime/model packs;
- configured remote providers;
- storage availability.

### 4. Resolve processing profile

For the requested workflow:

```bash
tsubame profile list --json
tsubame profile resolve "<profile>" --json
```

If no profile is chosen, prefer a safe/default profile appropriate to the platform but never silently change a previously explicit user selection.

### 5. Plan runtime/model downloads

```bash
tsubame runtime plan --profile "<profile>" --json
```

Present material downloads/cost before execution when appropriate.

Then:

```bash
tsubame runtime install --plan <plan-id>
```

### 6. Ensure Desktop

```bash
tsubame app ensure
```

The CLI should connect to or launch the installed Desktop.

## Common operations

See `references/commands.md`.

## Mutation rules

For edits to existing objects, pass `expected_revision` when the operation supports it.

If a revision conflict occurs:
1. re-read the object;
2. explain/reconcile the change;
3. retry only with a fresh expected revision.

## Provider secrets

Never print or request raw API keys if the Desktop can open its secure provider setup flow.

CLI/MCP output should expose redacted provider state and secret references only.

## Rights

Do not configure or generate an unauthorized real-person voice model. Tsubame's name is a homage to Yuzuki Tsubame and does not authorize use of her voice.
