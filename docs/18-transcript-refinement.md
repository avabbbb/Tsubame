# Transcript Refinement and Alignment

PR #9 adds the ASMR-specific review stage between ASR and translation:

```text
ASR
 → transcript.refine   (correction proposals, never auto-applied)
 → human review        (accept / skip, revision-checked)
 → speech.align        (timing-only result contract)
 → translation
```

## Refinement contract

Refinement resolves the `transcript.refine` capability through the same Provider Registry as every other feature. A paid API, an Alibaba/OpenAI-compatible endpoint and a local server such as LM Studio are the same code path: `POST <base_url>/chat/completions` on an `openai-compatible` Provider. There is no online/local branch and no model-name switch.

Request (internal, versioned):

```json
{
  "contract_version": 1,
  "language": "ja",
  "segments": [{ "segment_id": 12, "revision": 3, "start_ms": 1200, "end_ms": 2900, "text": "あきやまはるるです" }],
  "glossary": [{ "id": 1, "term": "秋山はるる", "aliases": ["Haruru Akiyama", "あきやまはるる"], "note": "CV" }]
}
```

Provider answer (JSON object, possibly wrapped in prose or code fences):

```json
{ "proposals": [{ "segment_id": 12, "text": "秋山はるるです", "reason": "glossary name", "confidence": 0.93 }] }
```

Segments are sent in batches of at most 40 lines. A failed batch leaves its lines unchanged and is reported; the run fails only when every batch fails.

## Conservative validation

A Provider's answer is candidate data. The Desktop keeps a proposal only when:

- it targets a Segment that was in the request;
- it is the first proposal for that Segment in the batch;
- its text is non-empty and differs from the current text;
- after applying glossary substitutions to the original, the character-level edit ratio is at most `0.4`.

Rewrites, translations, summaries and paraphrases fail the edit-ratio check and are dropped with a note. Glossary fixes (romanized or misheard names → canonical spelling) are not counted as edits, so they survive even when the name is long. Confidence is clamped to `0..1`.

## Proposal lifecycle

Proposals live in `refine_proposals`, never in `segments`.

```text
pending ──accept──▶ accepted   (Segment updated)
   │
   ├──skip────▶ rejected
   ├──new run─▶ superseded     (at most one pending proposal per Segment)
   └──accept on changed Segment ─▶ stale (Segment untouched)
```

Accept is revision-checked: the Segment's revision **and** source text must still match the proposal's `base_revision` / `original_text`. A human edit made after the check always wins; the proposal becomes `stale` and nothing is written.

An accepted proposal is a source edit:

```text
source_text           = proposed text
transcript_provenance = refine:<provider>:<model>
refine_provenance     = refine:<provider>:<model>
refine_confidence     = proposal confidence
asr_provenance        = unchanged
dirty translation/TTS/mix/subtitle = true
reviewed              = true
revision              = revision + 1
```

Re-running ASR replaces Segments, and their proposals are deleted with them (foreign-key cascade).

## Glossary

`glossary_terms` stores canonical spellings plus aliases (romanizations, kana readings, likely mis-hearings). It follows the project convention: Japanese canonical names with romanized aliases. Every refinement request carries the whole glossary. ASCII aliases match case-insensitively.

## Forced-alignment contract

`speech.align` adapters (worker, native helper or agent) return:

```json
{
  "contract_version": 1,
  "provider_id": "local.aligner",
  "model_id": "fa-1",
  "media_id": 7,
  "segments": [
    { "segment_id": 12, "base_revision": 4, "start_ms": 1240, "end_ms": 2860, "confidence": 0.88,
      "words": [{ "text": "秋山", "start_ms": 1240, "end_ms": 1610 }] }
  ],
  "notes": []
}
```

Validation rejects the whole result when any Segment is unknown, duplicated, stale (revision changed), out of order, has invalid timing, ends after the media, has confidence outside `0..1`, or has word timings outside its Segment.

Applying is one transaction and **timing-only**: start/end, `align_provenance = align:<provider>:<model>`, `align_confidence`, mix and subtitle dirty. TTS stays reusable, exactly like a manual timing edit. Any revision mismatch rolls back every Segment.

This PR ships the contract, validation and apply path (`apply_alignment_result`). Concrete aligner runtimes are installed through Runtime Packs (PR #12) and the ASMR-Dubber worker (PR #10).

## Confidence

Segments now persist `asr_confidence` (from the ASR result), `refine_confidence` and `align_confidence`. The Inspector shows each next to its provenance and highlights values under 60%.

## Desktop commands

| Command | Purpose |
| --- | --- |
| `refine_transcript` | run `transcript.refine` on a Track (optionally selected Segments) and store proposals |
| `list_refine_proposals` | pending proposals, or full history with `include_resolved` |
| `accept_refine_proposal` / `reject_refine_proposal` | revision-checked decision |
| `list_glossary` / `save_glossary_term` / `delete_glossary_term` | glossary |
| `apply_alignment_result` | validate and apply a `speech.align` result |

These map one-to-one onto future `$tsubame` CLI/MCP operations (PR #13).

## Deferred

- bundled aligner runtime and model download;
- word-level editing UI;
- per-Track glossaries;
- background job progress/cancel for long refinement runs.
