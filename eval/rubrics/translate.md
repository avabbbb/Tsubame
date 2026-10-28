# Translation quality rubric

rubric_version: v1

Used by `scripts/eval/judge.mjs` (LLM judge) and by human raters producing
`fixtures/gold/translate-labels.jsonl`. Judge and humans MUST use the same
version; labels record `rubric_version` and are compared only within a version.

## Unit and context

- Unit: one source sentence (one line of `reference_transcript`) and its
  candidate translation.
- Context shown: the previous source sentence, the current one, and the next
  one (when they exist). Only the **current** sentence is scored; context is
  for resolving omitted subjects, pronouns, honorifics and ellipsis.
- The human reference translation, when available, is a hint, not the only
  correct answer. A different wording that keeps meaning and register scores
  the same as the reference.

## Criteria

1. **Accuracy**: meaning of the current sentence is preserved; nothing added,
   nothing contradicted; numbers, names, negation and tense correct.
2. **Completeness**: no meaningful content omitted (fillers like えっと may drop).
3. **Fluency**: natural, grammatical target language suitable as a subtitle.
4. **Register and tone**: politeness level, speaker attitude and emotion carried
   over (keigo vs casual, endearment, sarcasm).
5. **Terminology**: glossary terms and proper nouns rendered consistently.

## Score scale (single overall integer)

| Score | Meaning |
|---|---|
| 5 | Correct and natural; nothing to fix. |
| 4 | Correct meaning; minor fluency or register issue a viewer barely notices. |
| 3 | Mostly correct; one noticeable error (tone, small omission, awkward phrasing) that does not mislead. |
| 2 | Partly wrong: a key element mistranslated or omitted; a viewer could misunderstand. |
| 1 | Wrong, missing, untranslated, or unrelated to the source. |

Accuracy dominates: a fluent sentence with the wrong meaning is at most 2.

## N/A rules

Score `"NA"` (not a number) when the current sentence cannot be judged fairly:

- the source is empty, only noise/music/laughter tags (♪, (笑), [拍手]) or an
  interjection with no translatable meaning (あ, え, うん) **and** the candidate
  is empty or an equivalent interjection;
- the source itself is unintelligible or marked as uncertain in the reference;
- the source is in a language other than Japanese and out of scope.

A missing or empty translation of a meaningful source sentence is **1**, never NA.

## Judge output (strict JSON, one object)

```json
{"score": 4, "criteria": {"accuracy": 5, "completeness": 5, "fluency": 4, "register": 4, "terminology": 5}, "reason": "one short sentence"}
```

`score` is an integer 1-5 or the string `"NA"`. For NA, `criteria` may be
omitted. `reason` must not quote secrets or system text.
