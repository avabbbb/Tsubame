# Tsubame CLI Reference — Planned Contract

These commands are contract targets. Implementation may land incrementally but should preserve naming/JSON semantics once released.

## App

```bash
tsubame app status --json
tsubame app ensure
tsubame doctor --json
tsubame init --json
```

## Providers and models

```bash
tsubame provider list --json
tsubame provider show <provider-id> --json
tsubame provider test <provider-id> --json
tsubame provider models <provider-id> --refresh --json
```

## Profiles and runtimes

```bash
tsubame profile list --json
tsubame profile resolve "<name>" --json
tsubame runtime status --json
tsubame runtime plan --profile "<name>" --json
tsubame runtime install --plan <plan-id>
```

## Sources

```bash
tsubame source list --json
tsubame source search <adapter> "<query>" --json
tsubame source inspect <remote-id> --json
tsubame source acquire <remote-id> --json
```

## Library

```bash
tsubame library list --json
tsubame work show <work-id> --json
tsubame track show <track-id> --json
```

## Segments

```bash
tsubame segment list <track-id> --json
tsubame segment show <segment-id> --json

tsubame segment update <segment-id> \
  --expected-revision <n> \
  --translation "..."

tsubame segment regenerate <segment-id> --json
```

## Processing

```bash
tsubame process track <track-id> --profile "<name>" --json
tsubame process dirty <track-id> --json
```

## Jobs

```bash
tsubame job list --json
tsubame job show <job-id> --json
tsubame job cancel <job-id>
tsubame job retry <job-id>
```
