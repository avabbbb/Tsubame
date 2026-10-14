# 20 — Single-sentence regenerate loop (PR plan #11)

Status: contract and implementation of [PR plan item #11](12-pr-plan.md) (GitHub PR #12,
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
3. A source-text edit never re-translates and never synthesizes automatically. It only
   marks the translation stale (`dirty_translation=1`, plus `dirty_tts`/`dirty_mix`/
   `dirty_subtitle`, see [Segment editor § Dirty dependency rules](14-segment-editor.md));
   TTS refuses a stale translation (no provider call). Downstream work starts only after
   the user re-translates, edits the translation, or confirms it with
   `confirm_translation` ("Keep translation"), and then regenerates explicitly.
4. Every write that a user can race is revision-checked (`expected_revision`); there is no
   silent last-write-wins.
5. No silent fallback: a skipped stage (e.g. spatialize without a worker) is reported in
   the job output with a reason, and the full-mix preview refuses (`spatial_pending`)
   rather than playing a raw clip where the user expects the spatial render.
6. Never re-synthesizes other sentences: a regenerate makes at most one provider call, for
   the regenerated sentence; later sentences only get new positions.
7. Never modifies the original audio and never overwrites a user-edited translation.

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
sentence A already produced for take `n` (a cache hit, no provider call).

### Ctrl/Cmd+Enter semantics

- A valid translation and no clip yet → synthesize (take 0, the PR #10 key).
- A clip exists, translation changed → a new take speaking the new translation.
- A clip exists, nothing changed → a forced new take (next take index, so a new cache key
  and a provider call; a sampling TTS gives a different reading).
- A/B, "Use this take" and revert only switch between existing takes (`set_active_take`);
  they never call TTS.

### Identical audio (dedupe)

Identical audio is never stored twice. On a cache miss the regenerate job hashes the
normalized WAV before storing it:

- a verified asset with the same sha256 (another take, another Segment) is reused; its file
  is shared and nothing new is written (`shared_asset: true`);
- if the Segment already has a take with exactly this audio, no take row is added
  (`new_take: false`, `duplicate_of_take: <index>`): that take becomes active again (or,
  when it already is, nothing changes: no revision bump, no replan).

A deterministic provider therefore cannot fill the five slots with copies.

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

- At most **5** takes per Segment are kept **in total** (`MAX_TAKES_PER_SEGMENT`): the
  active take (always, however old) plus the newest of the others. After each new take the
  older non-active rows beyond that are pruned; the active take is never evicted.
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
| `regenerate_segment(…, spatial_mode?)` | `spatial_mode` defaults to `source_trajectory` |
| `acknowledge_pushed(segment_ids)` | updated `Segment[]` |
| `confirm_translation(segment_id, expected_revision)` | updated `Segment` (see Product rules 3) |
| `preview_segment_context(segment_id, pre_roll_ms?, solo?, spatial_mode?, options?)` | `SegmentPreview` |

`confirm_translation`: revision-checked; clears `dirty_translation` and bumps `revision`;
the text, the clip and `dirty_tts` are untouched and no job starts. An empty translation
cannot be confirmed (`invalid_input`); an already clean Segment is a no-op.

`set_active_take` semantics:
- the key must belong to one of the Segment's take rows (`not_found` otherwise) and its
  asset must be verified (row + file + sha256; `asset_missing` otherwise);
- revision mismatch → `revision_conflict` (message contains "segment revision conflict",
  the phrase the Inspector matches on);
- writes `tts_asset_key`, `tts_provenance = tts:<provider>:<model>` of the asset,
  `dirty_mix=1`, `dirty_tts = (take.text is known and differs from the current
  normalized translation)`, bumps `revision`;
- recomputes placement downstream exactly like regenerate (see Replan) and returns the
  `plan_change`. It does not re-render spatial assets (no job). The export mixes a clip
  without a current render raw (PR #11 rule); the full-mix preview of the selected
  sentence refuses with `spatial_pending` instead (see Preview);
- bumps the Segment's generation token (see Stale jobs).
- Selecting the already-active take is a no-op that still checks the revision.

## Job `regenerate`

`regenerate_segment` runs as job kind `regenerate` through `jobs::run_to_completion`
(progress via `job://update`, cancel via `cancel_job`, retry via `retry_job` for failed +
retryable). Input `{segment_id, expected_revision, voice_profile_id, generation?,
spatial_mode}`; when
`voice_profile_id` is omitted the active take's voice profile (`tts_assets.voice_profile_id`)
is used, otherwise the job fails with `invalid_input` ("choose a voice profile").

Stages (`job.stage`, `progress_done/progress_total` = stage index / 3):

| # | stage | work |
|---|---|---|
| 0 | `tts` | resolve profile → `tts::prepare` with the next take index (revision check, refuses `dirty_translation`, empty text, `dub_skip`) → cache lookup → adapter → validate/store WAV → **commit** |
| 1 | `spatialize` | render of the new take in the job's `spatial_mode` (analysis first when not cached) |
| 2 | `replan` | `concurrent` mode only: render shifted downstream Segments that have no render under their new key |
| 3 | `done` | |

**Commit** (one DB lock, atomic with respect to other commands): check the generation
token and the revision, read the placement inputs, compute the previous plan, apply the
dedupe rule (see Identical audio), otherwise point the Segment at the new take
(`tts_asset_key`, `tts_provenance`, `dirty_tts=0`, `dirty_mix=1`, revision bump), insert
the take row, prune, then replan and persist the pushed flags. A failure at any point
before the commit (provider error, cancel, conflict, superseded) leaves the current take
active and usable.

### Stale jobs

A per-Segment generation token (`regen_generations(segment_id, generation)`) guards against
a slow job overwriting a newer user choice. `regenerate_segment` bumps it for every request
and passes the new value as `generation`; `set_active_take` bumps it inside its
transaction. The job's commit refuses with `superseded` (not retryable) when the token
moved on, i.e. the user started another regenerate or switched the active take while the
job ran. Its audio stays in the cache; no take row is written and nothing the user sees
changes. Inputs without `generation` (agent/CLI callers that serialize their own requests)
skip the check; the revision check still applies. Doing the plan diff at commit time means a cancel
or a spatial failure later can never leave a new clip without its pushed flags.

### Replan

- Placement is still derived, not persisted. The replan is incremental and never
  re-synthesizes: later sentences only get new time positions. At commit the previous plan is
  `place(sentences_before)` with default `PlacementParams` (the confirmed rules); the new
  plan comes from `placement::replan(previous, sentences_before, params, index,
  new_duration)`, which recomputes from the edited sentence and stops once the chain
  converges. Sentences before the edited one are never touched.
- `plan_change` = `{index, edited, recomputed_start, recomputed_end, moved: [...],
  pushed: [...]}` where `edited` is the edited Segment's new placement and each
  moved/pushed entry is `{segment_id, old_start_ms, new_start_ms, delta_ms, drift_ms,
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
  is already committed; the export mixes the raw clip and the full-mix preview reports
  `spatial_pending`); the stage reports `failed` with the error code/message.
- The job's `spatial_mode` (default `source_trajectory`, the confirmed default) decides
  what is rendered:
  - `source_trajectory`: only the regenerated sentence's new clip. Shifted sentences are
    **not** re-spatialized (the key has no `dub_start_ms`, their renders stay valid).
  - `concurrent`: the regenerated sentence plus every shifted sentence that has no verified
    render under its new key (new `dub_start_ms`). Sentences that did not move are never
    touched: a render key depends only on the sentence's own clip and window, so an
    overlapping neighbour that kept its position keeps its render.
- Output: `spatial: {status: done|skipped|failed, reason?, renders: [{segment_id, mode,
  render_key, cache_hit}], errors: [{segment_id?, code, message}]}`.

### `RegenerateOutcome`

`{segment, take: TakeInfo, take_index, new_take, duplicate_of_take, shared_asset,
cache_hit, previous_tts_asset_key, plan_change, spatial, pruned_takes: [take_index],
deleted_assets: [key]}`. `take`/`take_index` are the active take after the job (the new
take, or the existing take with identical audio).

### Failure modes

| Case | Result |
|---|---|
| Revision changed before commit | `revision_conflict` (not retryable); the synthesized asset stays in the content-addressed cache, no take row is written |
| Newer regenerate / take selection meanwhile | `superseded` (not retryable); same as above, the user's newer choice stands |
| Stale translation / empty text / `dub_skip` | `invalid_input` |
| Provider / model unavailable, HTTP errors | the `tts` error codes of docs/19 (`provider_unavailable`, `http`, …) |
| Cancel during `tts` | `cancelled`; nothing committed |
| Cancel after commit | `cancelled`; the new take stays active with its pushed flags (consistent state); missing renders: the export mixes raw clips, the full-mix preview reports `spatial_pending` |
| Spatial worker missing | stage `skipped`, job succeeds |
| Spatial worker error | stage `failed`, job succeeds |
| App restart mid-job | `interrupted` (retryable), as for every job |

## Preview

`preview_segment_context(segment_id, pre_roll_ms = 1000, solo = false, spatial_mode?,
options?)` renders a short WAV for immediate listening (not a job; runs on a blocking
thread). `options` are the export's `MixOptions` (default: the export defaults); the
Studio sends the same Limiter / Spatial follow settings it exports with. `spatial_mode`,
when given, overrides `options.spatial_mode`.

**Full mix (`solo = false`) = a window of the export.**

- Window: `start = max(0, src_start_ms − pre_roll_ms)`, `end = dub_end_ms + 1000 ms`
  (`PREVIEW_TAIL_MS`), clamped to the export's length. A Segment that is not placed
  (`no_clip`, `skipped`) fails with `not_placed`. `pre_roll_ms` must be in `0..=30000`.
- Same code path as `export_dub_mix`: `mix::resolve_export` builds the export's clip list
  (placement with `options.placement`, asset paths, verified spatial renders of the mode)
  and `mix::render_window` renders `[start, end)` of exactly what `mix::render` would write:
  every clip prepared whole (loudness from its full source sentence, fades, per-clip peak
  ceiling, dub gain), summed at its real position, then the same limiter. The pass starts
  early enough (no clip within 14 release time-constants before the window) that the
  limiter state at `start` equals the export's; only the window is written. Tests assert
  the preview equals the export slice sample for sample.
- Every placed dub that sounds in the window is in it (`layered_segment_ids`, placement
  order); `dub_skip` and unsynthesized sentences are not placed and never sound. A
  neighbour without a current render is mixed raw, exactly as the export does, and listed
  in `raw_segment_ids`. A missing clip file of any layered sentence fails (`asset_missing`)
  because the export would fail too.
- **No silent fallback** for the selected sentence: with `options.use_spatial` (Spatial
  follow, default on) and no verified render for its current key the preview fails with
  `spatial_pending` (retryable) instead of playing the raw clip. The UI shows the spatial
  progress / reason and offers "Dub only". With Spatial follow off the preview, like the
  export, mixes raw clips.
- `spatial: true|false` reports whether the selected sentence's render was used.

**Dub only (`solo = true`)**: the selected sentence's raw TTS clip as stored (its asset
path, no copy, no spatial audio, no original); `layered_segment_ids = [segment_id]`.

- Files go to `<app data>/assets/preview/` (`preview-<segment>-<uuid>.wav`); after each
  render only the newest 8 previews are kept (`MAX_PREVIEW_FILES`).
- Previewing clears the Segment's `dub_pushed_ms`.
- Result `SegmentPreview {segment_id, path, window_start_ms, window_end_ms, dub_start_ms,
  dub_end_ms, dub_offset_ms, solo, spatial, layered_segment_ids, raw_segment_ids,
  duration_ms}`.
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
- retention: five takes in total, an old active take is never evicted, assets still
  referenced by another Segment survive;
- dedupe: an unchanged regenerate with identical audio adds no take and no file; identical
  audio of an older take re-activates it; identical audio of another sentence shares its
  file;
- stale jobs: a newer request and a take selection both supersede an in-flight job, which
  then changes nothing;
- a source-only edit is refused by regenerate with zero provider calls until the
  translation is confirmed; `confirm_translation` is revision-checked and starts no TTS;
- `set_active_take` revision conflicts, unknown keys, dirty_tts recomputation;
- replan touches only downstream Segments and flags pushed ones; acknowledge clears;
- concurrent re-render targets are exactly the shifted Segments without a render under
  their new key;
- preview: window bounds (1 s pre-roll clamped at 0, 1 s tail), equals the export slice
  bit for bit (default, limiter engaged, limiter off), `spatial_pending` instead of a raw
  fallback, Dub only returns the raw clip, neighbours that sound are layered and
  `dub_skip` ones are not, cleanup; `mix::render_window` equals the export slice for
  default, loud, limiter-off and dub-stem renders;
- regenerate job progress stages and cancel; migration backfill idempotent;
- end to end: edit → regenerate → one provider call → replan → preview → revert, the
  user's translation and the original file untouched;
- with python3 + numpy available (real spatial worker): `source_trajectory` renders only
  the new clip; `concurrent` re-renders exactly the shifted sentence; one provider call in
  both (skipped otherwise).

## Frontend (Sentence Inspector)

- **One primary dub action.** The Inspector has a single button, labelled "Synthesize"
  while the Segment has no clip (`tts_asset_key` null) and "Regenerate" once it has one
  ("Save & …" while edits are pending), with the `Ctrl/Cmd+↵` hint. Both run
  `regenerate_segment` (a Segment without a take gets take 0, the PR #11 key, so a later
  `synthesize_segment` of take 0 is a cache hit); the PR #11 "Synthesize again" button and
  its single-clip / spatial-render players are gone (the takes list plays each clip, the
  context preview / "Dub only" plays the placed dub with its verified spatial render).
  `synthesize_segment` stays as a command for the agent/CLI surface.
- `Ctrl/Cmd+Enter` (focus in the Inspector, segment list/waveform or nowhere) triggers the
  same action: saves pending edits first, then runs `regenerate_segment` with
  `expected_revision`. A stale translation, an empty translation, `dub_skip` or (without a
  clip) no voice profile shows the reason and does not run.
- While the job runs the stages `tts → spatialize → replan` are shown with Cancel
  (`cancel_job`); on success the context preview plays automatically.
- Preview: "Play in context" (1 s before the sentence to 1 s after its dub, mixed with the
  export bar's Limiter / Spatial follow settings, which are persisted and shared) by
  default, "Dub only (raw voice)" toggle (`solo`); one `<audio>` element via
  `convertFileSrc`; starting any preview stops the previous one. When the preview reports
  `spatial_pending` the panel says the spatial audio is not ready and offers "Play dub
  only"; while a re-spatialize of the sentence runs a status line shows its progress and
  Play in context waits (Spatial follow on).
- "Keep translation" appears while the translation is stale after a source edit
  (`confirm_translation`); the blocker copy offers re-translate, edit or keep.
- A `superseded` regenerate shows a quiet note; a duplicate result names the identical
  take.
- Takes: up to 5, newest first, active marked; each plays solo and offers "Use this take"
  (`set_active_take`, revision-checked; a conflict reloads the Segment and explains). A/B
  switches between the active take and the compare take at the same playback position.
- Pushed marks: `dub_pushed_ms > 0` shows a yellow "Pushed +X.Xs" badge (tooltip "pushed
  later by X.Xs") in the segment list and a yellow waveform region; it clears after a
  context preview or the Inspector's OK (`acknowledge_pushed`). PR #10's red drift badges
  stay; the two differ in hue, outline and a leading arrow.
- A skipped spatial stage (`spatial_worker_unavailable`) is a subtle note, not an error; it
  says what to do (re-spatialize, Dub only, or Spatial follow off), not that a raw clip
  plays in its place.

## Scope

Only the regenerate loop. No model installer, no audio compression, no new provider
system (PR #11 adapters, voice profiles and BYOK are reused as they are).
- The voice (`voice_profile_id`): the Dub voice selector sits above the action. Without a
  clip the selected voice is sent (the selector's last choice is remembered for new
  sentences). With an active take the selector shows the take's voice and nothing is sent,
  so the backend keeps it; if the user changes the selector for this sentence the selected
  voice is sent instead (a note says the next take uses it) and the choice resets when
  another sentence is selected or the take is created. If the active take's voice is
  unknown (legacy empty) or no longer exists, the selected voice is sent.
- Visual hierarchy: Synthesize/Regenerate is the Inspector's only accent-filled primary
  button. The edit-save control at the bottom is a quiet grey "✓ Saved" status (not a
  button) while there are no unsaved edits; with pending edits (or an invalid time) it
  becomes a secondary "Save" button (saves without a new take; disabled while a time is
  invalid). `Ctrl/Cmd+↵` still saves pending edits first and then runs the action; there is
  no separate save shortcut.
- A "Spatial audio" section under it shows the active clip's format. The explicit
  re-spatialize (re-render the active clip's spatial audio without a new take) lives in
  that section's "More" (⋯) menu as "Re-spatialize only (no new take)", disabled with the
  reason while edits are unsaved or the clip is out of date; while it runs a status line
  shows progress and the menu offers "Cancel re-spatialize" (`cancel_job`). The menu is a
  minimal disclosure (`aria-haspopup="menu"`, `aria-expanded`; focus moves to the first
  item, ↑/↓/Home/End move, Esc closes and returns focus, a click outside closes). Listen
  via the context preview.
