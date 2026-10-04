# Agent Control and Tsubame Skill

## Goal

A local coding/general agent should be able to operate the installed Tsubame Desktop without clicking the UI.

The Desktop remains the canonical state owner.

## Control stack

```text
Local Agent
    │
  $tsubame Skill
    │
 ┌──┴───────────┐
 │              │
CLI            MCP
 │              │
 └────┬─────────┘
      │
  Local RPC
      │
Tsubame Desktop
      │
Operation Registry
```

## Why a Skill

The Skill provides host-agnostic instructions for:
- locating the install;
- bootstrapping when necessary;
- opening/connecting to Tsubame;
- running diagnostics;
- resolving/installing required models/runtimes;
- invoking stable CLI/MCP operations;
- understanding mutation/revision semantics.

It must not contain provider-specific business logic duplicated from the app.

## Proposed CLI

Examples:

```bash
tsubame doctor --json
tsubame app status --json
tsubame app ensure

tsubame provider list --json
tsubame provider models <provider> --refresh --json
tsubame profile resolve "Japanese · Best Quality" --json

tsubame runtime plan --profile "Japanese · Best Quality" --json
tsubame runtime install --plan <plan-id>

tsubame source search japanese-asmr "keyword" --json
tsubame source acquire <source-item-id>

tsubame library list --json
tsubame work show <work-id> --json
tsubame segment list <track-id> --json
tsubame segment update <segment-id> --expected-revision 7 --translation "..."
tsubame segment regenerate <segment-id>

tsubame job list --json
tsubame job cancel <job-id>
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
