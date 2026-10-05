# Vision

## One product, not three tools

Tsubame combines the useful parts of three existing product directions:

- a sentence-aware local media player;
- a recoverable AI transcription/translation/dubbing pipeline;
- a robust source/download manager.

The user should never feel that they are switching between “player”, “downloader” and “dubber”. A work enters the library once and accumulates assets and variants over time.

## The user mental model

A **Work** contains one or more **Tracks**. Each track can have:

- original audio/video;
- imported or generated subtitles;
- translated text;
- generated voice clips;
- bilingual mixes;
- exported deliverables.

AI processing is background work attached to those objects.

## Primary workflow

```text
Discover / Import
      ↓
Add to Library
      ↓
Play immediately
      ↓
Acquire/import/generate transcript
      ↓
Review and edit sentence timeline
      ↓
Translate
      ↓
Generate selected sentences or full dub
      ↓
Mix / export
```

At every stage the user can keep playing the original media.

## What makes Tsubame different

The differentiator is the combination of:

1. a real desktop media library;
2. a sentence-level timeline as canonical editable data;
3. modular ASR/refinement/translation/TTS providers;
4. local and BYOK processing profiles;
5. background, resumable jobs;
6. local-agent control through the same operation layer.

The core experience to prove first is:

> select one sentence → edit text/timing → preview → regenerate only that sentence → immediately hear the replacement.

## Initial platform

Windows x64 is the first production target because the local AI ecosystem and packaging path are strongest there. macOS follows with native Apple Speech integration as a first-class provider.
