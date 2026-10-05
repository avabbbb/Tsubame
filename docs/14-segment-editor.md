# Canonical Segment Editor

PR #5 makes `Segment` the editable timeline source of truth.

## Segment state

Each Segment persists:

- stable database ID;
- media ID;
- start/end milliseconds;
- source text;
- translated text;
- ordinal;
- optimistic `revision`;
- transcript / ASR / refine / translation / TTS provenance;
- dirty flags for translation, TTS, mix and subtitle output;
- reviewed state.

SRT and WebVTT are import/export formats, not canonical storage.

## Import

Supported initial formats:

- SubRip `.srt`;
- WebVTT `.vtt`.

Import replaces the selected Track's current Segment timeline after explicit UI confirmation.

Imported source text receives:

```text
transcript_provenance = import:srt
or
transcript_provenance = import:vtt
```

The imported timeline is immediately usable for playback/editing.

## Export

Exports are rebuilt from canonical Segments.

Text modes:

- Original;
- Translation, falling back to Original when untranslated;
- Bilingual.

Supported output:

- SRT;
- WebVTT.

## Dirty dependency rules

### Source text edit

```text
source changed
 → transcript provenance = human
 → translation dirty unless translation was edited in the same mutation
 → TTS dirty
 → mix dirty
 → subtitle output dirty
```

### Translation edit

```text
translation changed
 → translation provenance = human
 → translation itself is clean
 → TTS dirty
 → mix dirty
 → subtitle output dirty
```

### Timing-only edit

```text
timing changed
 → TTS remains reusable
 → mix dirty
 → subtitle output dirty
```

This rule is essential for later single-sentence regeneration.

## Revision conflicts

Every Segment mutation sends an `expected_revision`.

If the stored revision differs:
- the write fails;
- the UI fetches the latest Segment;
- the user can reload latest or explicitly keep their draft using the fresh revision.

No silent last-write-wins behavior.

## Undo / redo

Undo/redo applies persisted Segment snapshots through the same revision-checked mutation command. It is not a separate frontend-only database.

The history stack is session-local; each undo/redo still creates a new canonical Segment revision.

## Waveform

The waveform uses WaveSurfer with the existing Tsubame media element, not a second playback engine.

Regions map to canonical Segments:
- click region → select/seek Segment;
- drag region → move timing;
- drag edge → change start/end;
- region update → revision-checked Rust mutation.

Playback, speed and volume continue to belong to the global Tsubame player.

## Standards

WebVTT follows the W3C time-aligned cue model. Tsubame initially preserves cue text and timing; advanced WebVTT presentation settings are not part of the canonical Segment model.
