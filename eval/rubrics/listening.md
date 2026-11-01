# Dub listening rubric

rubric_version: v1

Used by human raters for the dub acceptance check (plan step 4). Results are recorded in eval/scorecard.json under the Correctness dimension (finding LISTEN-1) with the rubric_version, the TTS path (product Edge runtime pack or developer shim), the voice and the date.

## Sample

- Material: one exported dub (track 1, zh dub over ducked ja) produced through the product path: yuzuki translate run, tts synthesize, spatial render, mix export, with the Edge TTS runtime pack.
- Lines: 10 dub lines spread over the clip, chosen before listening (for example every n-th segment), plus any line the rater notices on a full pass.
- Playback: headphones, normal listening volume, no level changes between lines.

## Criteria (each 1-5 per line)

1. Intelligibility: every word of the dub is understandable on first listen.
2. Naturalness: prosody, pauses and pronunciation sound like natural Mandarin; no robotic jumps, clipped words or wrong readings of names and numbers.
3. Sync and pacing: the line starts near the original line and ends inside its slot; no overlap with the next line, no long silent gap.
4. Speed-up artifacts: lines that were sped up do not sound rushed, chipmunked or garbled.
5. Spatial placement: the dub sits where the original voice is (left/right, near/far) and follows its movement; no jumps between lines.
6. Mix balance: the dub is clearly on top; the ducked original is audible but never masks the dub; no pumping, clicks or clipping at line boundaries.

## Score scale

| Score | Meaning |
|---|---|
| 5 | Nothing to fix. |
| 4 | Minor issue a viewer barely notices. |
| 3 | One noticeable issue that does not stop understanding or immersion. |
| 2 | Distracting: the viewer loses words or immersion. |
| 1 | Unusable: unintelligible, missing, wrong place or broken audio. |

Score NA only when the source line is noise, music or a bare interjection and no dub was generated for it. A missing dub for a meaningful line is 1.

## Gate

- Per criterion: mean over the sampled lines.
- Pass for beta: every criterion mean at least 3.5 (scorecard beta_gate) and no line scored 1 on intelligibility or mix balance.
- Record each line as one JSON object: {"segment": 12, "scores": {"intelligibility": 5, "naturalness": 4, "sync": 4, "speedup": 5, "spatial": 4, "mix": 4}, "note": "one short sentence"}. Notes must not contain local paths, file names of the source or any secret.
