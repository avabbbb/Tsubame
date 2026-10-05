# Canonical Segment Editor

PR #5 turns Tsubame's sentence timeline into canonical editable production data.

## Segment contract

A Segment owns:

- stable database ID;
- media ID;
- start/end milliseconds;
- source text;
- translated text;
- revision;
- provenance slots;
- downstream dirty flags.

The player, subtitle import/export, waveform and future ASR/TTS workers all converge on this object.

## Revision safety

Every mutation carries `expected_revision`.

If the row changed after the caller read it, the mutation fails instead of silently overwriting a human/agent edit. The Inspector reloads the current row and surfaces a revision-conflict message.

## Dependency invalidation

Changes invalidate only downstream work.

### Source text edit

```text
source changed
 → translation dirty
 → TTS dirty
 → mix dirty
 → subtitle export dirty
```

### Translation edit

```text
translation changed
 → TTS dirty
 → mix dirty
 → subtitle export dirty
```

ASR is not invalidated.

### Timing edit

```text
start/end changed
 → mix dirty
 → subtitle export dirty
```

TTS is reusable when text/voice inputs did not change.

## Subtitle import

Supported formats:

- SubRip `.srt`
- WebVTT `.vtt`

Import modes:

- **Original** replaces the current Segment timeline with imported cue timing/text.
- **Translation** requires the same cue count as the canonical timeline and fills translated text.

This intentionally avoids pretending that two unrelated subtitle timelines can be losslessly aligned by index.

## Subtitle export

Tsubame can export:

- source;
- translation;
- bilingual text;

to SRT or WebVTT.

WebVTT output uses the standard `WEBVTT` header and dot-separated millisecond timestamps. SRT output uses sequential cue numbers and comma-separated millisecond timestamps.

## Waveform

WaveSurfer.js renders the waveform/timeline against Tsubame's existing media element.

Each Segment is a draggable/resizable region:
- drag region → shift timing;
- drag left/right handle → change start/end;
- click region → select the corresponding Segment;
- committed region edits go through the same revision-checked mutation path as the Inspector.

WaveSurfer is a rendering/editor dependency only. SQLite remains canonical.

## Undo / redo

The Inspector keeps a bounded in-session history for the currently selected Segment.

Undo/redo does not bypass persistence: it writes the previous/next snapshot through the normal revision-checked update operation, producing a new revision.

## Provenance

Initial provenance slots:

- ASR;
- refinement;
- translation;
- TTS.

Manual source edits mark ASR provenance `manual`. Manual translation edits mark translation provenance `manual`. Future providers/workers will write structured provenance identifiers through the same fields.
