# Notices

YUZUKI is an independent open-source project.

## Namesake

The name **YUZUKI** is a personal homage to voice actress **Yuzuki Tsubame (柚木つばめ)**.

YUZUKI is not affiliated with, endorsed by, sponsored by, or an official product of Yuzuki Tsubame.

No voice recording, voice model, likeness, logo or other branding asset of Yuzuki Tsubame is included by virtue of this naming choice.

## Software references and dependencies

### Reference only — no source incorporated

- ASPlayer — GPL-3.0 — https://github.com/yumili426/ASPlayer

ASPlayer may be consulted for product/interaction research only. YUZUKI does not copy, adapt, vendor, link, or distribute ASPlayer source code.

### Incorporated MIT-derived code

- ASMR-Dubber — MIT — https://github.com/EveningStudy/asmr-dubber (reference commit
  `2faea94fff102297d223996d43266d1cc20f6ca8`)

Ported or adapted into YUZUKI, each file keeping MIT attribution in its header:

- loudness matching and peak-ceiling logic (`audio.py`) → `app/src-tauri/src/mix.rs`;
- translation batching, prompt and validation rules (`translation.py`,
  `prompts/translation.md`) → `app/src-tauri/src/translate.rs`;
- OpenAI-compatible speech request shape and voice/style settings →
  `app/src-tauri/src/tts.rs`, `app/src-tauri/src/voice.rs`;
- spatial cue analysis and per-clip spatial follow rendering (`spatial_cues.py`,
  `spatial_rtf.py`) → `workers/dubber/spatial/`.

`app/src-tauri/src/placement.rs` re-implements the `sequential` timing idea of
`timing.py` without copying code.

ASMR-Dubber license:

```text
MIT License

Copyright (c) 2026 ASMR Dubber contributors

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
```

### MIT projects that may be integrated later

- asmr-downloader / ASMRoner — MIT — https://github.com/fireinrain/asmr-downloader

If code from this project is incorporated, its copyright and license notice must be
preserved as required.

## Rust dependencies

- hound — Apache-2.0 — https://github.com/ruuda/hound (WAV read/write for the offline dub mix)
- sha2 — MIT OR Apache-2.0 — https://github.com/RustCrypto/hashes (content-addressed cache keys)
- unicode-normalization — MIT OR Apache-2.0 — https://github.com/unicode-rs/unicode-normalization (NFC text for TTS cache keys)

## UI/library dependency

- wavesurfer.js — BSD-3-Clause — https://github.com/katspaugh/wavesurfer.js

YUZUKI uses wavesurfer.js for waveform visualization and its Regions/Timeline plugins. Preserve its BSD-3-Clause notice in distributions as required.
