# 19 — Dubbing pipeline (PR #10)

Status: contract frozen for PR #10 implementation. Reference lineage: ASMR-Dubber
(MIT, https://github.com/EveningStudy/asmr-dubber, commit `2faea94f`). Tsubame does
**not** wrap Dubber's pipeline, project format, CLI or UI. Each Dubber stage is
re-implemented as an independent capability behind Tsubame contracts, and evolved
where Dubber conflicts with Tsubame rules. Ported code keeps MIT attribution in the
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

Dropped from Dubber: ASR/refine/align (Tsubame has its own), voice cloning from the
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

## audio.mix

Offline export renders `original + Σ placed (spatialized) dub clips`. Original samples
are only summed with dub samples; a limiter, when enabled, acts on the sum bus only and
can be disabled. Loudness match per clip: target = source sentence active RMS + offset,
clamped. Output WAV/FLAC; also a dub-only stem export.

## Defaults pending user confirmation

anchor `end` + `beat_ms` 300; drift limit 4000 ms → flag only; spatial mode
`source_trajectory`; Chinese target `zh-Hans` by default with `zh-Hant` available;
first remote TTS = OpenAI-compatible adapter (provider-agnostic).
