# Upstream and Licensing

## ASPlayer

Repository: https://github.com/yumili426/ASPlayer

License: GPL-3.0.

Potentially reused lineage:
- Tauri/Vue/Rust desktop shell;
- media playback;
- SQLite media/subtitle state;
- sentence timeline;
- transcription event flow;
- selected learning/player utilities.

If ASPlayer source is incorporated, preserve copyright/license notices and publish the derivative under GPL-3.0 as required.

## ASMR-Dubber

Repository: https://github.com/EveningStudy/asmr-dubber

License: MIT.

Potentially reused/adapted lineage:
- recoverable project execution;
- ASR backend architecture;
- VAD/segmentation;
- review/alignment;
- translation;
- TTS backends;
- voice reference handling;
- per-sentence caching;
- timing/mix/subtitles;
- runtime/model-pack integrity patterns.

Preserve MIT notice for incorporated code.

## asmr-downloader / ASMRoner

Repository: https://github.com/fireinrain/asmr-downloader

License: MIT.

Primary role: architecture/reference for:
- source discovery;
- download queue;
- retry/backoff;
- rate limiting;
- sync state;
- resumability;
- metadata normalization.

Tsubame does not need to carry a Go runtime merely to reuse these ideas.

## Import policy

Before importing code:

1. record upstream repo + commit SHA;
2. identify copied/modified files;
3. preserve required notices;
4. add attribution to NOTICE where appropriate;
5. avoid copying unavailable/non-open source implementation;
6. keep upstream-specific behavior behind Tsubame contracts.

## Namesake

Tsubame is named as a personal homage to voice actress Yuzuki Tsubame (柚木つばめ). This is not an upstream software dependency and does not imply affiliation.
