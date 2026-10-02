# Domain Model

## Core entities

### Work

A logical title/album/work independent of source website.

Contains metadata, cover/artwork and Tracks.

### Track

One playable media unit inside a Work.

### Asset

A concrete file or addressable local artifact.

Kinds include:
- original audio;
- original video;
- subtitle;
- artwork;
- generated speech clip;
- mix;
- export.

### Segment

The first-class sentence/timeline object.

Minimum fields:

```text
id
track_id
start_ms
end_ms
source_text
translated_text
ordinal
revision

asr_provenance
refine_provenance
translation_provenance
tts_provenance

source_hash
translation_hash
timing_hash
tts_hash

generated_asset_id?
status
```

### Variant

A playback/export interpretation of a Track:
- original;
- original + translated subtitle;
- dubbed;
- bilingual mix;
- future variants.

### ProcessingProfile

Selects provider/model/parameters for each capability.

### ProviderConfig

Connection/runtime configuration separate from model choice.

### ModelDescriptor

A discovered or declared model with capability metadata.

### Job

Long-running processing/download work with progress, retries, cancellation and recovery state.

### RuntimePack

An app-managed executable/runtime dependency.

### SourceRecord

Maps a Work/Track back to a source adapter and remote identifiers.

## Dependency invalidation

A change should invalidate only dependent outputs.

Examples:

### Translation edit

```text
translated_text changed
  → TTS dirty
  → mix dirty
  → translated subtitle/export dirty
```

ASR does not rerun.

### Timing edit

```text
start/end changed
  → timing/alignment state updated
  → mix/subtitle export dirty
```

TTS audio may remain reusable when voice/text inputs are unchanged.

### Source text edit

```text
source_text changed
  → refinement provenance invalid
  → translation dirty
  → TTS dirty
  → mix dirty
```

## Concurrency

Mutations use revision checks.

A local agent editing a Segment should send `expected_revision`; stale updates fail instead of silently overwriting a human edit.

## Worker boundary

Workers receive immutable snapshots/IDs and return results/events. They never own canonical entity state.
