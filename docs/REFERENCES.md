# References and Dependency Boundaries

Tsubame distinguishes **product references** from **code dependencies**.

## Reference only

### ASPlayer

Repository: https://github.com/yumili426/ASPlayer  
License: GPL-3.0

ASPlayer is consulted only for product/interaction research, including ideas such as:

- sentence-aware playback;
- subtitle/timeline ergonomics;
- loop and navigation workflows;
- media-library interaction patterns.

**No ASPlayer source code is copied, adapted, vendored, linked or distributed by Tsubame.**

This boundary is required because Tsubame is MIT-licensed.

## MIT processing dependency candidate

### ASMR-Dubber

Repository: https://github.com/EveningStudy/asmr-dubber  
License: MIT

Potentially reusable capabilities behind Tsubame worker contracts:

- VAD/segmentation;
- ASR backend adapters;
- review/alignment;
- translation;
- TTS backends;
- voice-reference handling;
- per-sentence caching;
- timing/mixing/subtitles;
- runtime/model-pack patterns.

If source is incorporated, preserve its MIT copyright/license notice.

## MIT downloader/source reference

### asmr-downloader / ASMRoner

Repository: https://github.com/fireinrain/asmr-downloader  
License: MIT

Potentially reusable/reference areas:

- source discovery;
- download queue;
- retry/backoff;
- rate limiting;
- sync state;
- resumability;
- metadata normalization.

Tsubame may independently implement these ideas in Rust or incorporate MIT code where it remains architecturally appropriate.

## Import policy

Before importing third-party source:

1. verify the license is compatible with Tsubame's MIT distribution;
2. record repository + exact commit/tag;
3. identify copied/modified files;
4. preserve required notices;
5. keep third-party-specific behavior behind Tsubame contracts;
6. add tests around the boundary.

GPL/AGPL code must not be imported into the MIT Tsubame codebase without an explicit licensing decision that changes this policy.

## Namesake

Tsubame is named as a personal homage to voice actress Yuzuki Tsubame (柚木つばめ). This does not imply affiliation or grant rights to use her voice, recordings, likeness or branding.
