# Gold evaluation set (user-supplied)

This folder holds the **schema and instructions only**. No audio is committed,
and `fixtures/gold/audio/` is git-ignored. The quality checks in
`node scripts/eval/run.mjs` (dimension **处理质量 (Processing Quality)**) record
`skip` with a reason until you provide the files below. They never make up numbers.

## What you must supply

1. **10-20 Japanese audio clips** that you own (you recorded or made them) or that
   are clearly licensed for this use (CC0, CC-BY, CC-BY-SA, or written permission).
   - Put them in `fixtures/gold/audio/` (wav/flac/mp3/m4a/ogg/opus), about 5-60 s each.
   - No voices of real people without their permission. This includes
     Yuzuki Tsubame / 柚木つばめ, whose official site forbids AI use of her voice.
     See `docs/09-security-rights.md`.
2. **`fixtures/gold/manifest.json`**, which must validate against `manifest.schema.json`:
   - `id`: lowercase slug, unique.
   - `audio`: path relative to this folder, e.g. `audio/clip01.wav`.
   - `language`: always `"ja"`.
   - `reference_transcript`: verbatim human transcript, one sentence per line.
   - `reference_translation`: human translation, aligned line by line with the transcript.
   - `rights`: mandatory. `license`, `source`, `evaluation_permitted: true`,
     `real_person_voice` (`none` | `self` | `consented`), plus `attribution` for
     CC-BY or CC-BY-SA, and `consent_reference` for `permission` or `consented`.
3. **Optional, for translation-judge agreement:** `fixtures/gold/translate-labels.jsonl`,
   with 100-200 sentences that a human scored using `eval/rubrics/translate.md`.
   The line format is documented in `scripts/eval/kappa.mjs`.

## Example manifest

```json
{
  "schema_version": 1,
  "translation_language": "zh-Hans",
  "clips": [
    {
      "id": "clip01",
      "audio": "audio/clip01.wav",
      "language": "ja",
      "reference_transcript": "今日は来てくれてありがとう。\nおやすみ、また明日ね。",
      "reference_translation": "谢谢你今天能来。\n晚安，明天见。",
      "rights": {
        "license": "user-owned",
        "source": "Recorded by me, 2026-10-20",
        "evaluation_permitted": true,
        "real_person_voice": "self"
      }
    }
  ]
}
```

## Runtimes the checks need

- `asr_cer`: set `TSUBAME_FASTER_WHISPER_PYTHON` to a Python that has
  `faster-whisper` installed. The model comes from `YUZUKI_EVAL_ASR_MODEL`
  (default `large-v3`). This check runs only in full mode, or with `--quality`.
- `tts_roundtrip_cer`: needs the ASR runtime above, plus `YUZUKI_EVAL_TTS_CMD`.
  That command reads `{"text","language","out_path"}` JSON on stdin and writes a
  WAV file to `out_path`.
- Optional gates: `YUZUKI_EVAL_ASR_CER_MAX` and `YUZUKI_EVAL_TTS_CER_MAX`
  (for example 0.15). Without them both checks pass once they produce a number;
  with them a corpus CER above the gate fails. A clip the runtime cannot score
  always fails the check. Synthesized WAVs go to `eval/results/tts-roundtrip/`.
- translation judge agreement (check `quality-translate-judge`, full mode or `--quality`):
  needs `fixtures/gold/translate-labels.jsonl` (100-200 human-scored sentences) and an
  OpenAI-compatible chat endpoint for `scripts/eval/judge.mjs`:
  - `YUZUKI_JUDGE_BASE_URL`: API base ending in /v1 (a paid API, or a local LM Studio /
    Ollama / llama.cpp server).
  - `YUZUKI_JUDGE_MODEL`: model id.
  - `YUZUKI_JUDGE_AUTH`: `bearer` (default) or `none`; with `bearer`,
    `YUZUKI_JUDGE_API_KEY` is required. The key is read from the environment only and
    never written to results.
  The check passes when quadratic-weighted Cohen's kappa between judge and humans is
  at least 0.6 (`scripts/eval/kappa.mjs`). Run either script directly to see its usage.
