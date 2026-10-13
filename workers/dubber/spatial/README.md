# `audio.spatialize` worker (spatial follow)

Python worker that gives a mono/stereo TTS dub clip the stereo position of the
original speaker. Implements the `audio.spatialize` contract and worker protocol
v1 from [`docs/19-dubbing-pipeline.md`](../../../docs/19-dubbing-pipeline.md).
The worker never writes SQLite: Rust owns state, cache keys, asset moves and
orchestration; the worker writes only inside `input.out_dir`.

Ported from ASMR-Dubber (MIT), Copyright (c) 2026 ASMR Dubber contributors,
<https://github.com/EveningStudy/asmr-dubber> commit `2faea94f`
(`spatial_cues.py`, `spatial_rtf.py`, `experimental_mix.py:spatialize_clip`).
Every ported file carries the attribution header.

## Layout

| File | Role |
|---|---|
| `worker.py` | Entry point. Protocol v1 over stdio, ops `analyze` / `render`. |
| `_protocol.py` | Private minimal protocol v1 helper (to be replaced by the shared SDK `workers/dubber/tsubame_worker/`). |
| `analyze.py` | Track analysis → `SpatialTrack`, `spatial_track_key()`. |
| `render.py` | Per-clip render, hold-and-release trajectory, `render_key()`. |
| `_wav.py` | RIFF/RF64 WAV reader (memory-mapped; PCM 8/16/24/32, float 32/64) and float32 writer. |
| `tests/` | `unittest` suite (synthetic signals, golden vs Dubber, protocol). |

Dependencies: Python ≥ 3.9 and `numpy` (BSD-3-Clause), supplied by the runtime
pack (`requirements.txt`). No soundfile/soxr/scipy.

## Running

```sh
python3 workers/dubber/spatial/worker.py          # or: cd workers/dubber && python3 -m spatial.worker
```

## Protocol

Wire format is exactly protocol v1. Request `op` is the capability
`audio.spatialize`; the sub-operation is **`input.op`**: `"analyze"` or
`"render"`. Any other `op` / `input.op` → `error` `unsupported_op`.

Behaviour details (within the contract):
- `hello` is the first line; `capabilities[0].features` advertises
  `ops`, `modes`, `default_mode`, `default_strengths`, `analyzer_version`,
  `renderer_version`, `reference_channels` `[2]`, `clip_channels` `[1,2]`, `formats` `["wav"]`.
- Requests run one at a time in arrival order on a background thread; stdin
  stays responsive.
- `cancel` (running or queued job) is answered immediately with `error`
  `cancelled` (`retryable:false`); the job stops at its next cancellation
  point and no later message is emitted for that id. Unknown ids are ignored.
- `shutdown` cancels outstanding jobs (`cancelled`, `retryable:true`) and exits 0.
  stdin EOF without `shutdown` drains the queue, then exits 0.
- Outputs are written to a temp name in `out_dir` and renamed into place.
- Malformed lines are ignored with a note on stderr (no id to answer).

### `analyze` (once per media, cached by Rust)

```json
{"type":"request","id":"job-a","op":"audio.spatialize","input":{
  "op":"analyze",
  "reference_path":"/abs/reference.wav",
  "reference_kind":"mix",
  "reference_sha256":"9f2c…(64 hex)",
  "params":{},
  "out_dir":"/abs/job-a"}}
```

- `reference_kind`: `mix` | `vocals` (metadata + key only).
- `reference_sha256`: the host's hash; if omitted the worker hashes the file
  and emits a `log` warning.
- `params` (optional, all keys optional; unknown keys rejected): `hop_ms` 20,
  `frame_ms` 85 (→ next power of two samples), `bands` 28, `band_lo_hz` 100,
  `band_hi_hz` 18000 (≤ 0.44·rate), `band_halfwidth_ratio` 1.22, `ild_clip_db` 24,
  `itd_lo_hz` 180, `itd_hi_hz` 6500 (≤ 0.4·rate), `itd_max_ms` 0.9,
  `itd_reliable_coherence` 0.15, `activity_floor_db` −85, `activity_below_p90_db` 30,
  `color_clip_db` 6, `ild_hardpan_db` 12, `smoothing` `{itd_median:5, itd:7, ild:9, level:19, color:19}`.

Writes `out_dir/spatial_track.npz` and `out_dir/spatial_track.json`, then:

```json
{"type":"progress","id":"job-a","stage":"analyze","done":1,"total":2}
{"type":"progress","id":"job-a","stage":"write","done":2,"total":2}
{"type":"result","id":"job-a","output":{
  "op":"analyze",
  "spatial_track_key":"0bdfa712…",
  "analyzer_version":"spatial-analyze/1",
  "track_path":"/abs/job-a/spatial_track.npz",
  "summary_path":"/abs/job-a/spatial_track.json",
  "frames":150,"hop_ms":20,"sample_rate":48000,"duration_ms":3000,
  "summary":{"method":"reference-guided spectral ILD + GCC-PHAT ITD + relative level/color",
    "itd_convention":"positive = right ear delayed","ild_convention":"positive = left louder",
    "itd_ms_range":[-0.292,0.295],"median_ild_db_range":[-8.02,8.13],
    "reliable_itd_fraction":1.0,"active_fraction":1.0,"position_reliable_fraction":1.0,
    "frames":150,"hop_seconds":0.02,"duration_seconds":3.0,
    "distance":"relative loudness/color proxy only; no metric distance or room recovery"}}}
```

`spatial_track.npz` (no pickles; `np.load(..., allow_pickle=False)`):
`meta_json` (str: analyzer_version, sample_rate, hop, frame_size, params,
spatial_track_key, reference_sha256, reference_kind, duration_seconds),
`times` (F) s, `band_hz` (28), smoothed Dubber-equivalent `ild_db` (F×28),
`itd_seconds` (F), `level_db` (F), `color_db` (F×28); raw `ild_raw_db`,
`itd_raw_seconds`, `confidence`; masks `active`, `itd_reliable`,
`position_reliable`. About 370 B/frame (≈ 67 MB per hour, uncompressed).

### `render` (per sentence clip)

```json
{"type":"request","id":"job-r","op":"audio.spatialize","input":{
  "op":"render",
  "clip_path":"/abs/tts.wav",
  "reference_path":"/abs/reference.wav",
  "spatial_track_path":"/abs/spatial_track.npz",
  "spatial_track_key":"0bdfa712…",
  "tts_asset_key":"tts:5be1",
  "mode":"concurrent",
  "window":{"src_start_ms":500,"src_end_ms":2100,"dub_start_ms":2400},
  "strengths":{"rtf":1.0,"level":0.7,"color":0.35},
  "out_dir":"/abs/job-r",
  "output_name":"seg-42.wav"}}
```

- `mode`: `source_trajectory` (default) | `concurrent`.
- `window`: integer ms. `dub_start_ms` required only for `concurrent`.
- `strengths` (optional): `rtf` 0..1 (wet/dry), `level` 0..2, `color` 0..2.
- `spatial_track_key` / `reference_sha256` (optional) are checked against the
  track's metadata (`track_mismatch`). `tts_asset_key` (optional) enables
  `render_key` in the result.
- `output_name` (optional, default `spatial_clip.wav`): plain `*.wav` name.

```json
{"type":"progress","id":"job-r","stage":"render","done":1,"total":1}
{"type":"result","id":"job-r","output":{
  "op":"render","path":"/abs/job-r/seg-42.wav",
  "render_key":"13a72c70…",
  "sample_rate":24000,"channels":2,"frames":31200,"duration_ms":1300,
  "report":{"mode":"concurrent","renderer_version":"spatial-render/1","blocks":1,
    "fallback_blocks":{},"fallback_used":false,"context_ms":200,"hold_release_ms":150,
    "strengths":{"rtf":1.0,"level":0.7,"color":0.35},
    "mean_rtf_confidence":0.871,"reliable_bin_fraction":0.871,
    "held_frame_fraction":0.0,"input_channels":1}}}
```

Output: stereo IEEE-float32 WAV at the **clip's** sample rate with exactly the
clip's frame count (never time-stretched). Each fallback is reported in
`fallback_blocks` (`source_end`, `level_only`, `silent_block`, `silent_clip`) and
logged as a `warn` `log` message.

### Error codes

| code | retryable | meaning |
|---|---|---|
| `invalid_input` | no | missing/malformed field, bad params/strengths/window |
| `input_missing` | no | an input path does not exist |
| `unsupported_op` | no | `op` ≠ `audio.spatialize` or unknown `input.op` |
| `unsupported_format` | no | not a readable WAV |
| `reference_not_stereo` | no | reference is not 2-channel |
| `reference_too_short` | no | reference < 0.1 s |
| `reference_silent` | no | no active frame anywhere in the reference |
| `reference_invalid` / `clip_invalid` | no | non-finite samples, >2-channel clip |
| `track_invalid` / `track_version_mismatch` / `track_mismatch` | no | unreadable, stale or wrong track |
| `cancelled` | no (yes on shutdown) | job cancelled |
| `out_of_memory` | yes | MemoryError |
| `internal` | no | unexpected exception (traceback on stderr) |
| `runtime_missing` | no | numpy not installed (worker still says hello) |

### Cache keys

`‖` in the contract is realised as SHA-256 over a canonical JSON array
(sorted keys, `,`/`:` separators, UTF-8) with a domain tag, so fields can never
run into each other:

- `spatial_track_key = sha256(["tsubame.spatial_track.v1", reference_sha256(lowercase), reference_kind, resolved_params, analyzer_version])`
  — params are fully resolved against defaults, so `{}` and explicit defaults agree.
- `render_key = sha256(["tsubame.spatial_render.v1", tts_asset_key, spatial_track_key, mode, window, resolved_strengths, renderer_version])`
  — `window` is `{src_start_ms, src_end_ms}` in `source_trajectory` mode
  (timing edits do not re-render) and adds `dub_start_ms` in `concurrent` mode.

Integer-typed params (`hop_ms`, `frame_ms`, `bands`, `smoothing.*`) must be
integral and serialise as integers; all other numbers serialise as floats
(`0.9`, `18000.0`), so `20` and `20.0` give the same key. Python reference
implementations: `analyze.spatial_track_key`, `render.render_key`. Rust must
produce byte-identical keys; pinned vectors:

| call | sha256 |
|---|---|
| `spatial_track_key("0"*64, "mix", {}, "spatial-analyze/1")` | `1384a1916325c0effe1fca42d957658fd6f3b783aa22b24801fb6df2d0167923` |
| `render_key("tts:abc", "0"*64, "source_trajectory", {src 1000–2500, dub 2800}, defaults, "spatial-render/1")` | `a77b7081a3038637b6ff0199f11aad0eeaabddfd61f1e89c12c4b9a321afdb4e` |
| same with `"concurrent"` | `05fb51ff4c93a080b5d5ab75046f4a3a3493fa3331a85a4cb65974964c1edd8e` |

Canonical payload of the first vector:

```text
["tsubame.spatial_track.v1","000…000","mix",{"activity_below_p90_db":30.0,"activity_floor_db":-85.0,"band_halfwidth_ratio":1.22,"band_hi_hz":18000.0,"band_lo_hz":100.0,"bands":28,"color_clip_db":6.0,"frame_ms":85,"hop_ms":20,"ild_clip_db":24.0,"ild_hardpan_db":12.0,"itd_hi_hz":6500.0,"itd_lo_hz":180.0,"itd_max_ms":0.9,"itd_reliable_coherence":0.15,"smoothing":{"color":19,"ild":9,"itd":7,"itd_median":5,"level":19}},"spatial-analyze/1"]
```

## Algorithm

**Analysis** is Dubber's `spatial_cues.analyze` run once over the whole
reference: 20 ms hop, Hann frame of the next power of two ≥ 85 ms, 28
log-spaced ILD bands (100 Hz – min(18 kHz, 0.44·rate), ±1.22×, clipped ±24 dB),
GCC-PHAT ITD in 180–6500 Hz within ±0.9 ms with parabolic sub-sample peak,
confidence = lag-aligned L/R coherence, frame RMS level, relative colour
(±6 dB); active = level > max(−85, P90 − 30); ITD reliable = active ∧ coherence
≥ 0.15; smoothing ITD median 5 + mean 7, ILD 9, level/colour 19. The reference
is streamed from a memory-mapped WAV in 128-frame batches with vectorised FFTs
(≈ 40 s per hour of 48 kHz stereo on one core). Results match Dubber to ~1e-13.

**Render** is Dubber's complex-RTF renderer (`spatial_rtf.filters` +
`render_rtf`) fed from the cached track instead of re-analysing:
STFT 4096/512 (≥ 32 kHz) or 2048/256, ITD-derotated 2×2 covariance smoothed
0.16 s × 3 bins, principal-eigenvector L/R ratio with ILD prior on unreliable
bins, half the phase on each ear, energy-preserving gains; level envelope
(strength 0.7, −18…+12 dB) and colour (strength 0.35); 8 ms edge fades; 10 s
blocks with 0.2 s crossfaded pads, each block RMS-matched back to the dry clip;
`rtf` strength blends with the dry mono.

Tsubame evolutions:
- **Time mapping.** `source_trajectory`: clip sample *j* ↦ reference time
  `src_start + (src_end − src_start)·j/N` (the sentence path stretched to the clip).
  `concurrent`: *j* ↦ `dub_start + j/rate`; a block whose window has < 25 %
  active frames (or lies past the end of the reference) instead holds the
  **end of the source trajectory** (the last position-reliable frame inside the
  sentence window).
- **Hold, don't interpolate.** For the position (ITD, ILD), invalid frames hold
  the median of the last 5 valid raw frames; when valid frames resume the
  position releases with a 150 ms one-pole glide, then the Dubber smoothing is
  applied. ITD validity = `itd_reliable`; ILD validity = `position_reliable` =
  active ∧ (coherence ≥ 0.15 ∨ |broadband ILD| ≥ 12 dB), so hard-panned voices
  (one silent ear → zero coherence) still count. Level and colour keep Dubber's
  interpolation (they are not position).
- **Context.** Each block's reference window is extended by 200 ms on both
  sides (clamped to the file) so covariance smoothing is continuous at sentence
  edges; near-static mappings get 1 s of history.
- **Rates.** The reference window is FFT-resampled to the clip rate (no soxr).
- **Loudness** is left to `audio.mix`: no per-line peak ceiling (Dubber −9 dBFS);
  output is float so nothing clips.
- **Fallbacks.** Window < 0.1 s, silent, without active frames, or with no
  reliable position anywhere → Dubber's broadband L/R level-only fallback (no phase).

## Deviations from the frozen contract (to fold into docs/19 at integration)

1. Sub-operation selector is `input.op` (`analyze` | `render`); the contract does
   not name it.
2. `‖` concatenation is defined as a canonical JSON array with a domain tag (see above).
3. `render_key` drops `dub_start_ms` from `window` in `source_trajectory` mode
   (the contract's intent: "timing edits do not re-render").
4. Hold-and-release constants (5-frame median, 150 ms), context (200 ms), block
   size (10 s) and the concurrent activity threshold (25 %) are fixed and
   versioned through `renderer_version`, not request parameters, so they are not
   separate key fields.
5. Track asset is `spatial_track.npz` + `spatial_track.json` (contract); the
   analysis doc's packed `spatial_track.bin` layout was not used.
6. Added `position_reliable` (ILD validity incl. hard pan), not in Dubber.
7. stdin EOF drains queued work (batch mode); `shutdown` cancels.

## Limitations

- Stereo references only; mono/multichannel references are rejected.
- Single dominant source in two coherent channels; whole-mix references let SFX
  and ear-cleaning noise pollute cues (prefer `reference_kind: vocals`). No VAD
  gating input yet.
- `concurrent` fallback is decided per 10 s block, not per frame.
- Covariance (`filters`) is recomputed from the reference window on every
  render; only the cue track is cached.
- Peak memory ≈ 400 MB for a 10 s block at 48 kHz (same order as Dubber).
- WAV only (RIFF/RF64). Decoding other formats is the host's job.
- Not a neural renderer or 3-D tracker; distance is a loudness/colour proxy.

## Tests

```sh
cd workers/dubber
python3 -m unittest discover -s spatial/tests -t .
TSUBAME_GOLDEN_REPORT=1 python3 -m unittest spatial.tests.test_golden   # prints agreement numbers
```

`test_golden.py` imports Dubber from a local checkout (`TSUBAME_DUBBER_SRC`,
default `/workspace/work/asmr-dubber/src`) inside the test only and is skipped
when unavailable. Nothing from Dubber is vendored.
