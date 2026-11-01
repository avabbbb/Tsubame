# YUZUKI CLI Reference

The yuzuki binary (crates/yuzuki-cli) is a client of the running Desktop's local control endpoint. Each implemented command is one Operation Registry call; the Desktop does all validation. Installers ship yuzuki and yuzuki-mcp next to the Desktop executable. Commands marked planned do not exist yet: do not invoke them, follow SKILL.md "Implementation status".

Global flags: --json (one envelope on stdout: {"ok":true,"data":...} or {"ok":false,"error":{"code":...,"message":...}}), --control-file <path> (else YUZUKI_CONTROL_FILE, else the Desktop's app data directory), --launch (start the Desktop when it is not running; never implied).

Exit codes: 0 success, 1 other, 2 usage, 3 desktop_not_running / launch_failed, 4 unauthorized, 5 revision_conflict, 6 not_found, 7 invalid_input, 8 cancelled / superseded.

## App

| Command | Status | Operation |
|---|---|---|
| yuzuki app status --json | implemented | GET /v1/health (never launches) |
| yuzuki doctor --json | implemented | runtime.doctor (Runtime Packs only) |
| yuzuki ops list --json | implemented | GET /v1/ops (metadata and input schemas) |
| yuzuki call <operation> --input '<json>' --json | implemented | any registered operation |
| yuzuki app ensure --json | implemented | GET /v1/health; starts the Desktop only when nothing answers (data.launched) |
| yuzuki init --json | implemented | GET /v1/health, GET /v1/ops, runtime.doctor; reports cli, mcp (path, mcpServers config), desktop; never launches without --launch |

## Providers and models

| Command | Status | Operation |
|---|---|---|
| yuzuki call provider.list --json | implemented | provider.list (secret references only, no raw keys) |
| yuzuki provider list --json | planned | provider.list |
| yuzuki provider show <provider-id> --json | planned | none |
| yuzuki provider test <provider-id> --json | planned | none |
| yuzuki provider models <provider-id> --refresh --json | planned | none |

## Profiles and runtimes

| Command | Status | Operation |
|---|---|---|
| yuzuki runtime plan --provider-id <id> [--model-id <id>] --json | implemented | runtime.plan |
| yuzuki runtime install --provider-id <id> [--model-id <id>] --json | implemented | runtime.install (queues a job) |
| yuzuki profile list --json | planned | none |
| yuzuki profile resolve "<name>" --json | planned | none |
| yuzuki runtime status --json | planned | use yuzuki doctor --json |
| yuzuki runtime plan --profile "<name>" --json | planned | none (keyed by provider/model today) |

## Sources

| Command | Status | Operation |
|---|---|---|
| yuzuki source list --json | planned | none |
| yuzuki source search <adapter> "<query>" --json | planned | none |
| yuzuki source inspect <remote-id> --json | planned | none |
| yuzuki source acquire <remote-id> --json | planned | none |

## Library

| Command | Status | Operation |
|---|---|---|
| yuzuki media list --json | implemented | media.list |
| yuzuki media import-folder <path> --json | implemented | media.import_folder (recursive; same-name srt/vtt/lrc sidecars seed segments; returns the media items) |
| yuzuki library list --json | planned | none |
| yuzuki work show <work-id> --json | planned | none |
| yuzuki track show <track-id> --json | planned | none |

## Segments

| Command | Status | Operation |
|---|---|---|
| yuzuki segment list --media-id <id> --json | implemented | segment.list |
| yuzuki segment update --id <id> --expected-revision <n> [--source-text ..] [--translated-text ..] [--start-ms n] [--end-ms n] --json | implemented | segment.update (omitted fields keep their value) |
| yuzuki segment regenerate --segment-id <id> --expected-revision <n> [--voice-profile-id ..] [--spatial-mode ..] --json | implemented | segment.regenerate (waits for the job) |
| yuzuki segment show <segment-id> --json | planned | none |

## Processing

| Command | Status | Operation |
|---|---|---|
| yuzuki asr run --media-id <id> --provider-id <id> [--model-id ..] [--language ..] [--prompt ..] --json | implemented | asr.run (queues an asr job and returns it; follow with job list --media-id <id> --kind asr; success replaces that media's segments) |
| yuzuki translate run --media-id <id> --provider-id <id> --model-id <id> --target-language <lang> [--source-language ..] [--segment-id n ...] [--overwrite-human-edits] --json | implemented | translate.run (queues a translate job; follow with job list --media-id <id> --kind translate; human-edited translations are kept unless --overwrite-human-edits) |
| yuzuki tts synthesize --segment-id <id> --expected-revision <n> [--voice-profile-id ..] [--take n] --json | implemented | tts.synthesize (queues a tts job; default voice profile when omitted) |
| yuzuki spatial analyze --media-id <id> --json | implemented | spatial.analyze (queues the media's spatial analysis) |
| yuzuki spatial render --segment-id <id> [--mode source_trajectory\|concurrent] --json | implemented | spatial.render (queues one Segment's render) |
| yuzuki mix export --media-id <id> --output <file.wav> [--options '<MixOptions json>'] --json | implemented | mix.export (queues the mix job; output's parent directory must exist; relative paths resolve against the current directory) |
| yuzuki process track <track-id> --profile "<name>" --json | planned | none |
| yuzuki process dirty <track-id> --json | planned | none |

## Jobs

| Command | Status | Operation |
|---|---|---|
| yuzuki job list [--media-id n] [--segment-id n] [--kind ..] [--status ..] [--limit n] --json | implemented | job.list |
| yuzuki job cancel --id <job-id> --json | implemented | job.cancel |
| yuzuki job show <job-id> --json | planned | none (job list returns all) |
| yuzuki job retry <job-id> | planned | none |
