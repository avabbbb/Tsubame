# Yuzuki CLI Reference — Planned Contract

These commands are contract targets. Implementation may land incrementally but should preserve naming/JSON semantics once released.

## App

```bash
yuzuki app status --json
yuzuki app ensure
yuzuki doctor --json
yuzuki init --json
```

## Providers and models

```bash
yuzuki provider list --json
yuzuki provider show <provider-id> --json
yuzuki provider test <provider-id> --json
yuzuki provider models <provider-id> --refresh --json
```

## Profiles and runtimes

```bash
yuzuki profile list --json
yuzuki profile resolve "<name>" --json
yuzuki runtime status --json
yuzuki runtime plan --profile "<name>" --json
yuzuki runtime install --plan <plan-id>
```

## Sources

```bash
yuzuki source list --json
yuzuki source search <adapter> "<query>" --json
yuzuki source inspect <remote-id> --json
yuzuki source acquire <remote-id> --json
```

## Library

```bash
yuzuki library list --json
yuzuki work show <work-id> --json
yuzuki track show <track-id> --json
```

## Segments

```bash
yuzuki segment list <track-id> --json
yuzuki segment show <segment-id> --json

yuzuki segment update <segment-id> \
  --expected-revision <n> \
  --translation "..."

yuzuki segment regenerate <segment-id> --json
```

## Processing

```bash
yuzuki process track <track-id> --profile "<name>" --json
yuzuki process dirty <track-id> --json
```

## Jobs

```bash
yuzuki job list --json
yuzuki job show <job-id> --json
yuzuki job cancel <job-id>
yuzuki job retry <job-id>
```
