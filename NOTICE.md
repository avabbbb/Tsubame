# Notices

Tsubame is an independent open-source project.

## Namesake

The name **Tsubame** is a personal homage to voice actress **Yuzuki Tsubame (柚木つばめ)**.

Tsubame is not affiliated with, endorsed by, sponsored by, or an official product of Yuzuki Tsubame.

No voice recording, voice model, likeness, logo or other branding asset of Yuzuki Tsubame is included by virtue of this naming choice.

## Software references and dependencies

### Reference only — no source incorporated

- ASPlayer — GPL-3.0 — https://github.com/yumili426/ASPlayer

ASPlayer may be consulted for product/interaction research only. Tsubame does not copy, adapt, vendor, link, or distribute ASPlayer source code.

### MIT projects that may be integrated later

- ASMR-Dubber — MIT — https://github.com/EveningStudy/asmr-dubber
- asmr-downloader / ASMRoner — MIT — https://github.com/fireinrain/asmr-downloader

If code from MIT projects is incorporated, their copyright and license notices must be preserved as required.

Incorporated: ASMR-Dubber loudness matching and peak-ceiling logic (`audio.py`) is ported
in `app/src-tauri/src/mix.rs` with its MIT attribution in the file header.


## Rust dependencies

- hound — Apache-2.0 — https://github.com/ruuda/hound (WAV read/write for the offline dub mix)

## UI/library dependency

- wavesurfer.js — BSD-3-Clause — https://github.com/katspaugh/wavesurfer.js

Tsubame uses wavesurfer.js for waveform visualization and its Regions/Timeline plugins. Preserve its BSD-3-Clause notice in distributions as required.
