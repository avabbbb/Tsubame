# Product Contract

## Product statement

**Tsubame is a local-first ASMR library, player and AI dubbing workbench.**

It can discover or import media, persist it in a local library, play it continuously, build a sentence timeline, translate/edit individual sentences, synthesize selected or full speech, and export derived assets.

## Core surfaces

- **Home** — continue listening, recent works, active jobs.
- **Library** — local canonical media collection.
- **Discover** — pluggable source adapters.
- **Downloads** — resumable acquisition queue.
- **Studio** — transcript/timeline/edit/dub workbench.
- **Jobs** — processing/download history and recovery.
- **Settings** — providers, models, runtimes, storage, sources and agent control.

## User promises

### No setup tax

A normal user installs Tsubame Desktop and uses it. They do not manually install a Python environment, Rust toolchain, Node, FFmpeg, CUDA SDK or model server.

### Play before process

Import/downloaded media should be playable as soon as possible. AI processing is optional and asynchronous.

### Sentence-level control

Every transcript sentence has:
- stable ID;
- start/end;
- source text;
- translation;
- processing status;
- optional generated voice asset;
- revision and dependency signatures.

### Provider freedom

A user may choose different providers/models for ASR, refinement, alignment, translation and TTS. The UI exposes capabilities, not vendor lock-in.

### BYOK

Cloud providers use user-owned keys. Supported providers should discover accessible models after connection and refresh them on demand.

### Local-first

Media, timeline state, playback state, job state and user edits are local by default.

## Out of scope for the first production milestone

- GalGame presentation mode;
- social/community features;
- cloud account sync;
- DRM/paywall bypass;
- training custom voice models inside Tsubame;
- mobile clients.

## Success metric for the first real build

A fresh Windows user can:

1. install Tsubame;
2. import one local Japanese audio/video file;
3. choose or install an ASR provider;
4. generate sentence subtitles;
5. edit one sentence;
6. configure a translation provider through BYOK;
7. translate it;
8. configure/install a supported TTS runtime;
9. regenerate only that sentence;
10. continue playback using the generated variant.

No developer toolchain is required.
