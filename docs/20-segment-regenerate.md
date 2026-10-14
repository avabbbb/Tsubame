# 20 — Single-sentence regenerate loop (PR plan #11)

Status: contract for the backend of [PR plan item #11](12-pr-plan.md) (GitHub PR #12,
branch `feat/segment-regenerate`). Builds on the [dubbing pipeline](19-dubbing-pipeline.md)
(`tts.rs`, `placement.rs`, `spatial.rs`, `mix.rs`, `jobs.rs`) and the
[Segment editor](14-segment-editor.md) dirty rules. Product decisions confirmed by the
product owner on 2026-10-13.

```text
select Segment → edit → Ctrl/Cmd+Enter → regenerate_segment
  → tts (new take) → spatialize → replan downstream → preview_segment_context
```

## Product rules

1. Regenerating one sentence synthesizes **only that sentence**. Every other sentence
   keeps its clip; downstream sentences are only re-placed (and re-rendered where a
   render key says so).
2. Placement rules are unchanged: the dub starts at the original sentence's end + 300 ms,
   clips are never sped up, stretched or cut, a dub waits on collision, drift > 4000 ms is
   flagged only (`drift_exceeded`).
3. A source-text edit never re-translates automatically. It only marks the translation
   stale (`dirty_translation=1`, plus `dirty_tts`/`dirty_mix`/`dirty_subtitle`, see
   [Segment editor § Dirty dependency rules](14-segment-editor.md)); TTS refuses a stale
   translation, so the user re-translates or edits the translation first.
4. Every write that a user can race is revision-checked (`expected_revision`); there is no
   silent last-write-wins.
5. No silent fallback: a skipped stage (e.g. spatialize without a worker) is reported in
   the job output with a reason.

## Takes

A *take* is one synthesized variant of a sentence. Regenerate always creates a new take;
the previous takes stay available for A/B comparison and revert.

### Cache key

The TTS cache key (`tts::cache_key`, docs/19 § speech.tts implementation notes) already
carries `take` as its last length-prefixed field. Take `0` therefore yields exactly the key
that every existing clip was stored under (existing caches stay valid; pinned by a golden
test), and take `n > 0` differs only by that field. The key stays content-addressed:
identical inputs with the same take index share one asset across Segments ("cache
sharing").

Consequence: regenerating sentence B to take `n` reuses the asset that an identical
sentence A already produced for take `n`. That is still a different take than B's previous
one, so the user always hears something new.

### Data model

```text
tts_takes(
  segment_id      INTEGER NOT NULL,
  take_index      INTEGER NOT NULL,      -- the take field of the cache key
  tts_asset_key   TEXT NOT NULL,         -- → tts_assets.key
  text            TEXT NOT NULL DEFAULT '',  -- normalized text the take speaks
  voice_profile_id TEXT NOT NULL DEFAULT '',
  created_at      TEXT NOT NULL DEFAULT (datetime('now')),
  PRIMARY KEY (segment_id, take_index)
)
segments.dub_pushed_ms INTEGER NOT NULL DEFAULT 0   -- see Replan
```

- The **active take** is the one `segments.tts_asset_key` points at. There is no separate
  "active" column, so placement, mix and spatial keep reading `tts_asset_key` unchanged.
- Migration (idempotent, in `tts::migrate`): creates the table and backfills one take-0
  row for every Segment that already has a `tts_asset_key` and no take rows (`text` = the
  current normalized translation when `dirty_tts=0`, else empty = unknown).
- `synthesize_segment` (PR #10 command) also records its take row, so both paths keep the
  table consistent.

### Retention

- At most **5** takes per Segment are kept (`MAX_TAKES_PER_SEGMENT`). After each new take
  the oldest take rows (lowest `take_index`) beyond the newest 5 are pruned; the active
  take is never pruned.
- A pruned take's asset (row in `tts_assets` + file) is deleted **only** when nothing
  references its key any more: no `tts_takes` row of any Segment and no
  `segments.tts_asset_key`. Shared assets survive.
- Next take index = `max(take_index) + 1` for the Segment (0 when it has no takes).
- Limit: spatial renders derived from a pruned asset are not garbage-collected in this PR
  (they are keyed, never mixed for a key that is no longer current).

### Commands

| Command | Result |
|---|---|
| `list_takes(segment_id)` | `TakeInfo[]`, newest first: `{segment_id, take_index, tts_asset_key, text, voice_profile_id, duration_ms, path (absolute, null if file missing), available, active, created_at}` |
| `set_active_take(segment_id, take_key, expected_revision)` | `ActiveTakeOutcome {segment, take, plan_change}` |
| `regenerate_segment(segment_id, expected_revision, voice_profile_id?)` | job output `RegenerateOutcome` |
| `acknowledge_pushed(segment_ids)` | updated `Segment[]` |
| `preview_segment_context(segment_id, pre_roll_ms?, solo?, spatial_mode?)` | `SegmentPreview` |

`set_active_take` semantics:
- the key must belong to one of the Segment's take rows (`not_found` otherwise) and its
  asset must be verified (row + file + sha256; `asset_missing` otherwise);
- revision mismatch → `revision_conflict` (message contains "segment revision conflict",
  the phrase the Inspector matches on);
- writes `tts_asset_key`, `tts_provenance = tts:<provider>:<model>` of the asset,
  `dirty_mix=1`, `dirty_tts = (take.text is known and differs from the current
  normalized translation)`, bumps `revision`;
- recomputes placement downstream exactly like regenerate (see Replan) and returns the
  `plan_change`. It does not re-render spatial assets (no job); renders missing for a
  current key fall back to the raw clip in mix/preview as in PR #10.
- Selecting the already-active take is a no-op that still checks the revision.

## Job `regenerate`

`regenerate_segment` runs as job kind `regenerate` through `jobs::run_to_completion`
(progress via `job://update`, cancel via `cancel_job`, retry via `retry_job` for failed +
retryable). Input `{segment_id, expected_revision, voice_profile_id}`; when
`voice_profile_id` is omitted the active take's voice profile (`tts_assets.voice_profile_id`)
is used, otherwise the job fails with `invalid_input` ("choose a voice profile").

Stages (`job.stage`, `progress_done/progress_total` = stage index / 3):

| # | stage | work |
|---|---|---|
| 0 | `tts` | resolve profile → `tts::prepare` with the next take index (revision check, refuses `dirty_translation`, empty text, `dub_skip`) → cache lookup → adapter → validate/store WAV → **commit** |
| 1 | `spatialize` | `source_trajectory` render of the new take (analysis first when not cached); a `concurrent` render too when the previous take had one |
| 2 | `replan` | re-render `concurrent` renders of downstream Segments whose start moved |
| 3 | `done` | |

**Commit** (one DB lock, atomic with respect to other commands): read the placement inputs,
compute the previous plan, point the Segment at the new take (`tts_asset_key`,
`tts_provenance`, `dirty_tts=0`, `dirty_mix=1`, revision bump), insert the take row, prune,
then replan and persist the pushed flags. Doing the plan diff at commit time means a cancel
or a spatial failure later can never leave a new clip without its pushed flags.

### Replan

- Placement is still derived, not persisted. At commit the previous plan is
  `place(sentences_before)` with default `PlacementParams` (the confirmed rules); the new
  plan comes from `placement::replan(previous, sentences_before, params, index,
  new_duration)`, which recomputes from the edited sentence and stops once the chain
  converges. Sentences before the edited one are never touched.
- `plan_change` = `{index, recomputed: [start, end), moved: [...], pushed: [...]}` where
  each entry is `{segment_id, old_start_ms, new_start_ms, delta_ms, drift_ms,
  drift_exceeded}`. `moved` = downstream Segments whose `dub_start_ms` changed; `pushed`
  = the subset that moved **later**.
- Persistence: `segments.dub_pushed_ms` accumulates the displacement since the user last
  looked at the Segment: `new = previous + delta`, cleared when ≤ 0. It is a UI
  annotation, not canonical content, so writing it does **not** bump `revision` and never
  causes a revision conflict. The UI shows `dub_pushed_ms > 0` as "pushed" (yellow) until
  `acknowledge_pushed(segment_ids)` or `preview_segment_context` on that Segment clears it.
- Only regenerate and `set_active_take` set the flag. Timing edits and `dub_skip` toggles
  also move downstream placements but do not flag them in this PR.

### Spatialize

- Runs only when the spatial worker is available: the Python program resolves on `PATH`
  (or `TSUBAME_PYTHON`) and the worker script exists. Otherwise the stage is
  `skipped` with reason `spatial_worker_unavailable`.
- A worker/analysis/render failure other than cancel does not fail the job (the new take
  is already committed and mix falls back to the raw clip); the stage reports `failed`
  with the error code/message.
- Render keys decide what is redone: `source_trajectory` keys do not contain
  `dub_start_ms`, so moved downstream Segments are **not** re-rendered in that mode. A
  `concurrent` render of a moved Segment is re-rendered only when a verified render exists
  under its old key (old `dub_start_ms`) and none exists under its new key.
- Output: `spatial: {status: done|skipped|failed, reason?, renders: [{segment_id, mode,
  render_key, cache_hit}]}`.

### `RegenerateOutcome`

`{segment, take: TakeInfo, cache_hit, previous_tts_asset_key, plan_change, spatial,
pruned_takes: [take_index], deleted_assets: [key]}`.

### Failure modes

| Case | Result |
|---|---|
| Revision changed before commit | `revision_conflict` (not retryable); the synthesized asset stays in the content-addressed cache, no take row is written |
| Stale translation / empty text / `dub_skip` | `invalid_input` |
| Provider / model unavailable, HTTP errors | the `tts` error codes of docs/19 (`provider_unavailable`, `http`, …) |
| Cancel during `tts` | `cancelled`; nothing committed |
| Cancel after commit | `cancelled`; the new take stays active with its pushed flags (consistent state), missing renders fall back to raw clips |
| Spatial worker missing | stage `skipped`, job succeeds |
| Spatial worker error | stage `failed`, job succeeds |
| App restart mid-job | `interrupted` (retryable), as for every job |

## Preview

`preview_segment_context(segment_id, pre_roll_ms = 1000, solo = false, spatial_mode =
source_trajectory)` renders a short WAV for immediate listening (not a job; runs on a
blocking thread):

- Window: `start = max(0, src_start_ms − pre_roll_ms)`, `end = dub_end_ms + 250 ms` tail
  (`PREVIEW_TAIL_MS`), using the Segment's current placement (default params). A Segment
  that is not placed (`no_clip`, `skipped`) fails with `not_placed`. `pre_roll_ms` must be
  in `0..=30000`.
- The original window is cropped (same WAV format) into the preview area; when the window
  reaches past the end of the original the render extends with silence on the original side
  (mix rule).
- The Segment's dub is layered at `dub_offset_ms = dub_start_ms − start` through
  `mix::render` (same loudness match, fades, limiter defaults as export). The spatial render
  for the clip's current render key is used when one is verified, otherwise the raw TTS clip
  (`spatial: true|false` in the result). Only this Segment's dub is layered; neighbouring
  dubs are not included in v1.
- `solo = true` renders the dub only (`ExportKind::DubStem`, 32-bit float, same window).
- Files go to `<app data>/assets/preview/` (`preview-<segment>-<uuid>.wav`); after each
  render only the newest 8 previews are kept (`MAX_PREVIEW_FILES`).
- Previewing clears the Segment's `dub_pushed_ms`.
- Result `SegmentPreview {segment_id, path, window_start_ms, window_end_ms, dub_start_ms,
  dub_end_ms, dub_offset_ms, solo, spatial, duration_ms}`.
- WAV only, like mix (`unsupported_source_format` for compressed originals).

## Agent / CLI surface

All commands are Tauri commands with JSON in/out and run regenerate through the jobs
table, so the agent control surface (docs/07) can drive the same loop; no GUI automation.

## Security / rights

No new provider or voice paths: regenerate uses the existing voice profiles (`preset` /
`design`, no cloning) and BYOK secret lookup. Previews and takes stay in the app data
directory.

## Tests (backend)

- take 0 key equals the PR #10 key (golden), take n ≠ take 0;
- pruning keeps 5 takes, never the active one, and keeps assets still referenced by
  another Segment;
- `set_active_take` revision conflicts, unknown keys, dirty_tts recomputation;
- replan touches only downstream Segments and flags pushed ones; acknowledge clears;
- preview window bounds and clamping at 0; solo vs mix output; cleanup;
- regenerate job progress stages and cancel; migration backfill idempotent.
