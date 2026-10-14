# 19 — Dubbing pipeline (PR #10)

Status: contract frozen for PR #10 implementation. Reference lineage: ASMR-Dubber
(MIT, https://github.com/EveningStudy/asmr-dubber, commit `2faea94f`). YUZUKI does
**not** wrap Dubber's pipeline, project format, CLI or UI. Each Dubber stage is
re-implemented as an independent capability behind YUZUKI contracts, and evolved
where Dubber conflicts with YUZUKI rules. Ported code keeps MIT attribution in the
file header.

## Product rules (non-negotiable)

1. The original audio is never altered. The dub is a separate layer summed on top.
2. Dub clips play at natural speed. No time-stretch, no speed-up, no truncation.
3. On collision the dub waits; it never cuts the original or another dub clip.
4. Per-sentence work is the unit: changing one sentence re-runs only that sentence.
5. Voices are `preset` or `design` (text-described). No `speech.voice_clone` of the
   original performer in this pipeline.
6. Workers never write SQLite. Rust owns state, assets, cache and orchestration.

## Stage map

| Stage | Capability | Runs in | Module |
|---|---|---|---|
| Translation | `text.translate` | Rust (OpenAI-compatible) | `translate.rs` |
| Speech synthesis | `speech.tts` | Rust (remote HTTP); local models via worker | `tts.rs`, `voice.rs` |
| Placement | pure function `dub.place` (not a provider) | Rust | `placement.rs` |
| Spatial follow | `audio.spatialize` | Python worker v1 (numpy), Rust later | `workers/dubber/spatial/` |
| Mix / export | `audio.mix` | Rust | `mix.rs` |
| Jobs + worker protocol | — | Rust + Python SDK | `jobs.rs`, `worker.rs`, `workers/dubber/tsubame_worker/` |

Dropped from Dubber: ASR/refine/align (YUZUKI has its own), voice cloning from the
original (`voice_reference.py` default), replacement bed (alters original), speed-up
fitting (`timing.py` `fit_window`), `autoflow/`, Gradio UI.

## Worker protocol v1 (JSON Lines over stdio)

One JSON object per line. stdout carries protocol only; logs go to stderr.

```text
worker → {"type":"hello","protocol":1,"worker":"<name>","version":"x.y.z",
          "capabilities":[{"id":"audio.spatialize","features":{}}],"devices":["cpu"]}
host   → {"type":"request","id":"<job-id>","op":"<capability>","input":{...}}
worker → {"type":"progress","id":"<job-id>","stage":"<name>","done":N,"total":M}
worker → {"type":"log","id":"<job-id>","level":"info|warn|error","message":"..."}
worker → {"type":"result","id":"<job-id>","output":{...}}
worker → {"type":"error","id":"<job-id>","code":"<slug>","retryable":bool,"message":"..."}
host   → {"type":"cancel","id":"<job-id>"}
host   → {"type":"shutdown"}
```

- Host rejects a worker whose `protocol` differs or whose hello lacks the requested
  capability (capability handshake).
- Inputs are immutable: host records sha256 of every input file before the request and
  re-checks after; a mismatch fails the job.
- Workers write only inside `input.out_dir`. Host validates outputs and atomically
  moves them into the asset store.
- Cancel: worker must answer with `error` code `cancelled` within 2 s, else host kills.

## Jobs

Table `jobs(id TEXT PK, kind, media_id, segment_id NULL, status
queued|running|succeeded|failed|cancelled, progress_done, progress_total, stage,
error_code, error_message, retryable, attempt, input_json, output_json, created_at,
updated_at)`. Tauri event `job://update` carries the row. Commands: `list_jobs`,
`cancel_job`, `retry_job` (only `failed` + `retryable`, increments `attempt`).

## text.translate

Request (built by Rust): batch of `{id, revision, source_text, context_before,
context_after}`, `source_language`, `target_language` (`en`, `zh-Hans`, `zh-Hant`),
glossary terms (shared with PR #9 `glossary_terms`). Batches: 16 sentences or 4000
chars. Response: `{"translations":[{"id":..,"text":".."}]}`.
Validation: unknown/duplicate/empty ids dropped; a batch that fails is reported and does
not abort others; an over-long response triggers a split-in-half retry. Apply only if
the segment `revision` still matches; sets `translated_text`,
`translation_provenance = translate:<provider>:<model>`, `dirty_translation=0`,
`dirty_tts=1`, bumps `revision`. Human-edited translations are not overwritten unless
explicitly requested.

## speech.tts and voices

`voice_profiles(id, name, kind preset|design, provider_id, model_id, preset_voice,
design_prompt, language, params_json, created_at, updated_at)`.

Adapters (selected by provider kind, not model name):
- `openai-compatible` `/audio/speech` with `voice` (preset) and optional `instructions`
  (design prompt where supported).
- mock adapter for tests.

Cache key (content-addressed, timing-independent):
`sha256(provider_id ‖ model_id ‖ adapter_version ‖ normalized_text ‖ language ‖
voice_profile_hash ‖ params_json ‖ take)`.
`tts_assets(key PK, path, sha256, duration_ms, sample_rate, channels, provider_id,
model_id, voice_profile_id, created_at)`; `segments` gains `tts_asset_key TEXT`,
`dub_skip INTEGER DEFAULT 0`. Command `synthesize_segment(segment_id,
expected_revision, voice_profile_id, take?)` → sets `tts_asset_key`,
`tts_provenance`, `dirty_tts=0`, `dirty_mix=1`. Editing timing never changes the key.

### speech.tts implementation notes (`tts.rs`, `voice.rs`)

Recorded decisions and deviations from the frozen text above:

- **Key encoding.** Each `‖` field is length-prefixed (u64 BE) after the domain
  tag `tsubame.speech.tts.key.v1`; `normalized_text` is the Segment's
  `translated_text` in NFC with whitespace runs collapsed; `params_json` is
  canonical JSON (keys sorted recursively); `take` defaults to `0`.
- **`voice_profile_hash`** = sha256 of canonical `{v, kind, preset_voice,
  design_prompt (design only)}`. Name, id and timestamps are excluded (renaming a
  voice never invalidates clips); provider, model, language and params are their
  own key fields.
- **Adapter versions.** `openai-compatible/audio-speech@1`, `mock@1`. The mock
  adapter is not selectable by Provider kind.
- **Request shape.** `POST {base_url}/audio/speech` with `model`, `input`,
  `voice` (when the profile has one), `instructions` (design voices only),
  `response_format: "wav"`; profile params are merged at top level but cannot
  override those fields. A design voice may also carry an optional base
  `voice` because some OpenAI-compatible endpoints require one. Providers that
  reject `instructions` fail visibly; there is no silent retry without it.
- **No cloning.** `voice_profiles.kind` is constrained to `preset|design`;
  `params_json` containing reference-audio keys (`reference_audio`,
  `speaker_wav`, `prompt_audio`, …) is rejected.
- **Text source.** Synthesis reads `translated_text` only (no fallback to source
  text) and refuses when it is empty, when `dirty_translation=1`, or when
  `dub_skip=1`.
- **Assets.** WAV is validated (PCM/float, consistent block align, >0 frames;
  streaming headers with 0/0xFFFFFFFF sizes tolerated) and rewritten as
  `RIFF+fmt+data` with exact sizes before hashing. Stored atomically (temp file in
  the same directory, fsync, rename) at `<app data>/assets/tts/<key[0..2]>/<key>.wav`.
  `tts_assets.path` is **relative to `<app data>/assets`** so the data directory
  can move. A cache hit requires the row, the file and a matching sha256;
  anything else re-synthesizes.
- **Segment write.** `synthesize_segment` bumps `revision` (like every Segment
  write) unless the Segment already points at the same clean clip. Also exposed:
  `get_tts_asset(segment_id)` and `set_segment_dub_skip(segment_id,
  expected_revision, skip)` (bumps `revision`, sets `dirty_mix=1`).
- **Placement / mix API.** `tts::resolve_segment_tts_asset(&MediaDb,
  &AssetStore, segment_id) -> Result<Option<ResolvedTtsAsset>, TtsError>` returns
  the absolute path, `duration_ms`, format and `dub_skip`; it errors when the
  recorded file is missing.
- **Jobs.** `synthesize_segment` runs as a `tts` job; synthesis reports through
  the shared `jobs::ProgressSink` (implemented by `JobHandle`).

## dub.place (pure Rust)

Input: ordered sentences `{segment_id, src_start_ms, src_end_ms, clip_duration_ms,
skip}` and `PlacementParams {anchor: start|end (default end), beat_ms (default 300),
min_gap_ms (default 120), max_drift_ms (default 4000)}`.
Algorithm: `planned = anchor_time + beat_ms`; `start = max(planned,
previous_dub_end + min_gap_ms)`; `drift = start - planned`. Clip duration is never
changed. A sentence with `drift > max_drift_ms` gets flag `drift_exceeded` (UI offers
shorter translation or skip). Output per sentence `{segment_id, dub_start_ms,
dub_end_ms, drift_ms, flags[]}`. Incremental: after one clip changes, recompute from
that sentence until drift returns to the previous plan.

Implementation (`placement.rs`):
- `planned` is clamped at 0 (a negative `beat_ms` with `anchor=start` near t=0).
  `beat_ms` ∈ ±30 000, `min_gap_ms` ∈ 0..60 000, `max_drift_ms` ≥ 0; drift exactly
  equal to `max_drift_ms` is not flagged.
- Sentences with `dub_skip=1` get flag `skipped`; sentences without a TTS asset (or a
  zero-length one) get `no_clip`. Neither occupies time. Their row keeps the contract
  shape with `dub_start_ms = dub_end_ms = planned`, `drift_ms = 0`; consumers test the
  flags (`Placement::is_placed`).
- `replan(previous_plan, sentences, params, changed_index, new_duration)` recomputes
  from the changed sentence and stops when the chain carry (end of the last placed dub)
  equals the previous plan's carry; it returns the plan plus the recomputed index range
  and always equals a full `place()`.
- The plan is derived, not persisted: `plan_dub(media_id, params?)` recomputes it from
  Segments (`start_ms`, `end_ms`, `dub_skip`) joined to `tts_assets.duration_ms` via
  `tts_asset_key`, in `start_ms, ordinal, id` order. A key without an asset row reads as
  `no_clip`. Before the TTS migration exists the reader treats every Segment as `no_clip`.

## audio.spatialize

Two layers, ported from Dubber `spatial_cues.py` / `spatial_rtf.py`:
1. Track analysis (once per media, cached): 20 ms hop, 28-band ILD, ITD (180–6500 Hz,
   ±0.9 ms) with confidence, level and colour, smoothed. Asset `spatial_track.npz` +
   summary JSON. Key `sha256(reference_sha256 ‖ reference_kind ‖ params ‖
   analyzer_version)`.
2. Per-clip render: mono/stereo TTS clip → stereo clip following a reference window.
   `mode: source_trajectory` (default; original sentence path stretched to clip length,
   timing edits do not re-render) or `concurrent` (original position at dub play time).
   Low-confidence frames hold the last reliable position. Key `sha256(tts_asset_key ‖
   spatial_track_key ‖ mode ‖ window ‖ strengths ‖ renderer_version)`.

### Host side (`spatial.rs`)

- Job kind `spatial`, registered at startup. Commands `analyze_spatial(media_id)` and
  `spatialize_segment(segment_id, mode?)` (mode defaults to `source_trajectory`) run
  through the job runner; progress/cancel via `job://update` / `cancel_job`.
- The reference is the media file itself (`reference_kind: mix`, default analysis
  params); the host hashes it to derive `spatial_track_key`. Analysis is cached once per
  media by that key, renders per clip by `render_key`. A worker result whose key differs
  from the host's fails with `spatial_key_mismatch`.
- Outputs are promoted into the asset store at `spatial/tracks/<key[0..2]>/<key>.npz`
  (+ `.json` summary) and `spatial/renders/<key[0..2]>/<key>.wav` and recorded in
  `spatial_assets`; a verified cache row (row + file + sha256) skips the worker.
- Worker process: `TSUBAME_PYTHON` or `python3`; script `TSUBAME_SPATIAL_WORKER` or
  `workers/dubber/spatial/worker.py` relative to `CARGO_MANIFEST_DIR` (dev-time path;
  packaging belongs to the runtime bootstrap, PR plan #12).
- Mix lookup: `MixOptions.use_spatial` (default on) with `spatial_mode` (default
  `source_trajectory`). For each clip the mixer computes the clip's *current* render key
  (its TTS asset key, the media's current track key, the mode, its window, default
  strengths); when a verified render exists it is mixed instead of the raw TTS clip,
  otherwise the raw clip is used. The report carries `spatial_clips`.
- `concurrent` renders are planned with the default placement params, so the mix only
  finds them when it is exported with default placement params too.

## audio.mix

Offline export renders `original + Σ placed (spatialized) dub clips`. Original samples
are only summed with dub samples; a limiter, when enabled, acts on the sum bus only and
can be disabled. Loudness match per clip: target = source sentence active RMS + offset,
clamped. Output WAV/FLAC; also a dub-only stem export.

Implementation (`mix.rs`, command `export_dub_mix(media_id, output_path, options?)`):
- Frames where no dub clip contributes are written from the original's raw samples
  (bit-identical). With the limiter disabled the whole output equals the original
  except where clips play (tested). The mix keeps the original's WAV format (rate,
  channels, bit depth, int/float).
- Limiter (default on, `limiter: false` disables): instant attack, 50 ms release,
  ceiling -1 dBFS, linked across channels, applied to the sum only where a clip
  contributes plus its release tail (until gain is within 1e-6 of unity). It only stops
  the dub from pushing the sum above `max(ceiling, original peak)`, so it never limits
  what the original alone already does. The stem never passes through it.
- Loudness (ported from Dubber `active_rms_dbfs` / `prepare_chinese_clip`): source level
  = gated active RMS (50 ms frames, relative 20 dB gate) of the original's mono mix over
  the sentence window; target = level + `relative_loudness_db` (-8) clamped to
  [`min_active_rms_dbfs` -42, `max_active_rms_dbfs` -30]; a silent source targets the
  minimum; one gain capped at `max_boost_db` (+12); 8 ms edge fades; per-clip peak
  ceiling `clip_peak_dbfs` (-9); then `dub_gain_db`. None of this changes clip length.
- Clips are resampled to the original rate with a band-limited windowed-sinc resampler
  (rate change only; output length = round(len · to/from)) and channel-mapped (mono →
  both channels of stereo, centre of 3+ channels; extra channels dropped; N → mono by
  averaging). A decoded clip whose duration differs from `tts_assets.duration_ms` by
  more than 50 ms fails with `clip_duration_mismatch` instead of being mixed.
- When the last dub ends after the original, the output is extended with silence on the
  original side instead of truncating the dub.
- `kind: dub_stem` writes the dub layer only, 32-bit float WAV at the original rate and
  channel count, same length as the mix.
- Output goes to `<output>.partial` and is renamed on success; failure or cancel removes
  it. Writing over the original media file is refused.
- `export_dub_mix` runs as a `mix` job: progress/cancel through the shared
  `jobs::ProgressSink` (`progress(stage, done, total)`, `is_cancelled()`), observed via
  `job://update` and cancelled with `cancel_job`.

Deviations / limits of this implementation (PR #10):
- Input and output are **WAV only** (`hound`, Apache-2.0). Compressed originals (mp3,
  m4a, flac…) fail with `unsupported_source_format`; decoding them needs the app-managed
  FFmpeg runtime or a permissively licensed decoder (Symphonia is MPL-2.0 and was not
  added). FLAC output is not implemented. RIFF limits a file to 4 GiB.
- Spatialized clips are looked up by render key (see audio.spatialize § Host side); a
  clip without a current render is mixed from its raw TTS asset.
- Export does not clear `dirty_mix`.

## Defaults (confirmed by product owner 2026-10-09)

- Placement: the dub starts at the original sentence's end + 300 ms (`anchor: end`,
  `beat_ms: 300`).
- Spatial follow: default mode `source_trajectory` (`concurrent` available).
- Drift: cumulative drift > 4000 ms is **flagged only** (`drift_exceeded`); the user
  chooses to shorten the translation or skip the sentence. Nothing is cut or sped up.
- Chinese target: `zh-Hans` by default, `zh-Hant` available.
- First remote TTS: the OpenAI-compatible adapter (provider-agnostic).

## Implementation deviations (PR #10)

Where the implementation refines or departs from the frozen text above:

- **Translate context is batch-level.** `context_before` / `context_after` are the 4
  non-empty source lines (`CONTEXT_LINES`) just outside each batch, not per sentence.
- **Empty translation = intentional filler.** A Segment whose machine translation came
  back empty (`translate:*` provenance, empty text) is a deliberate filler: it is not
  re-selected for translation and TTS skips it.
- **TTS refuses dirty translations** (`dirty_translation=1`), empty text and `dub_skip`
  sentences; it never falls back to source text.
- **TTS cache key encoding**: length-prefixed fields after a domain tag, NFC text, canonical
  params (see speech.tts implementation notes).
- **Worker output paths** are found by key convention: `path` / `*_path` (string) and
  `paths` / `*_paths` (list) at any depth of `output`; each must resolve to an existing
  regular file inside `out_dir`.
- **`out_dir` is host-set**: the host creates it and overwrites any caller value in
  `input`.
- **Interrupted jobs**: on startup every `queued` / `running` row is marked `failed` with
  `interrupted` (retryable), instead of resuming.
- **Cancelled translation** surfaces as an error (`cancelled`, retryable); batches applied
  before the cancel are kept.
- **Limiter tail**: the limiter acts only where a clip contributes plus its release tail;
  elsewhere the original is copied bit-identically.
- **WAV only** for mix input/output and spatial reference/clips; compressed sources fail
  with `unsupported_source_format`; FLAC output is not implemented.
- **Spatial reference hashing cost**: the host sha256-hashes the whole media file to
  derive the track key, about three full reads per render (analysis check, render, mix
  lookup). Follow-up: cache the hash by path + size + mtime.

Single-sentence regenerate, takes, downstream replan and context preview build on this
pipeline: see [Segment regenerate](20-segment-regenerate.md).
