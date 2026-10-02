# Agent Control and Yuzuki Skill

## Goal

A local coding/general agent should be able to operate the installed Yuzuki Desktop without clicking the UI.

The Desktop remains the canonical state owner.

## Control stack

```text
Local Agent
    │
  $yuzuki Skill
    │
 ┌──┴───────────┐
 │              │
CLI            MCP
 │              │
 └────┬─────────┘
      │
  Local RPC
      │
Yuzuki Desktop
      │
Operation Registry
```

## Why a Skill

The Skill provides host-agnostic instructions for:
- locating the install;
- bootstrapping when necessary;
- opening/connecting to Yuzuki;
- running diagnostics;
- resolving/installing required models/runtimes;
- invoking stable CLI/MCP operations;
- understanding mutation/revision semantics.

It must not contain provider-specific business logic duplicated from the app.

## Proposed CLI

Examples:

```bash
yuzuki doctor --json
yuzuki app status --json
yuzuki app ensure

yuzuki provider list --json
yuzuki provider models <provider> --refresh --json
yuzuki profile resolve "Japanese · Best Quality" --json

yuzuki runtime plan --profile "Japanese · Best Quality" --json
yuzuki runtime install --plan <plan-id>

yuzuki source search japanese-asmr "keyword" --json
yuzuki source acquire <source-item-id>

yuzuki library list --json
yuzuki work show <work-id> --json
yuzuki segment list <track-id> --json
yuzuki segment update <segment-id> --expected-revision 7 --translation "..."
yuzuki segment regenerate <segment-id>

yuzuki job list --json
yuzuki job cancel <job-id>
```

## MCP

MCP tools should map onto the same Operation Registry as CLI/UI.

Do not create an MCP-only business path.

## Mutation policy

Read-only calls execute directly.

Mutating calls use:
- typed arguments;
- expected revision where relevant;
- explicit operation result;
- audit entry.

Potentially costly actions should expose a plan:
- large runtime/model downloads;
- paid remote inference;
- bulk processing/acquisition.

## Desktop interaction

Agents should not need screen automation.

A running app exposes a localhost-only control endpoint or equivalent authenticated local IPC. The CLI starts/connects to the Desktop and calls the Operation Registry.

## Unattended workflows

Supported long-term examples:
- import a folder and generate subtitles for new Japanese tracks;
- use a chosen profile on all selected tracks;
- regenerate only dirty sentences;
- monitor download/processing jobs;
- export completed subtitle/dub variants.

Unattended mode must still respect cost, rights and mutation policies.
