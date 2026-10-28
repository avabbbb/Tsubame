# Modular ASR Providers

PR #7 makes transcription a replaceable \`speech.asr\` capability.

## Contract

Every Provider adapter receives a media/request description and returns **candidate Segments**:

\`\`\`json
{
  "contract_version": 1,
  "adapter_id": "faster-whisper",
  "model_id": "large-v3",
  "language": "ja",
  "segments": [
    {
      "start_ms": 120,
      "end_ms": 1940,
      "text": "こんばんは",
      "confidence": 0.94
    }
  ],
  "notes": []
}
\`\`\`

Provider adapters do **not** own YUZUKI state and do not write SQLite.

The Desktop validates the result, converts it to canonical Segment rows, records ASR provenance, and invalidates only downstream work.

\`\`\`text
media
  ↓
ASR adapter
  ↓
AsrResult (candidate data)
  ↓
validation
  ↓
YUZUKI Desktop
  ↓
canonical SQLite Segment[]
\`\`\`

## Providers and internal adapters

### Faster-Whisper

Provider: `local.faster-whisper` · execution: `local_runtime` · adapter: isolated Python worker.

Reference:
- https://github.com/SYSTRAN/faster-whisper

The worker uses \`WhisperModel\` and returns its segment start/end/text output.

Runtime/model installation is deliberately not owned by this PR. During development the adapter becomes ready when \`TSUBAME_FASTER_WHISPER_PYTHON\` points to a Python runtime containing faster-whisper. PR #11 will replace this development bridge with app-managed Runtime Packs.

Default model hint: \`large-v3\`.

### SenseVoice / FunASR

Execution: isolated Python worker.

References:
- https://github.com/modelscope/FunASR
- https://github.com/QwenAudio/SenseVoice

SenseVoice/FunASR can produce rich speech-understanding output. YUZUKI normalizes only what it can represent honestly in the ASR contract.

If a checkpoint/runtime exposes timestamps without reliable sentence grouping, the worker does **not invent sentence boundaries**. It returns a whole-span Segment and records a note. Refinement/alignment is a later stage.

Runtime: the app-managed `runtime.funasr` pack (embeddable CPython 3.12.0, funasr 1.4.16, CPU torch; see [Runtime bootstrap](06-runtime-bootstrap.md)). `TSUBAME_FUNASR_PYTHON` overrides it and is always reported. The worker keeps stdout for the JSON result only (model logs go to stderr), splits speech with `fsmn-vad`, transcribes each span with SenseVoice, applies `rich_transcription_postprocess`, and turns ITN off for Japanese and Korean. Segment times are VAD speech boundaries, recorded in the result notes.

Default model hint: \`iic/SenseVoiceSmall\`.

### Apple SpeechAnalyzer

Provider: `local.apple-speech` · execution: `native_os` · adapter: macOS native helper.

Reference:
- https://developer.apple.com/videos/play/wwdc2025/277/

Apple's SpeechAnalyzer/SpeechTranscriber is treated as a local \`speech.asr\` engine, not an OpenAI-compatible provider.

The Rust adapter expects a versioned helper that reads the same JSON request/result contract. During development it is discovered via \`TSUBAME_APPLE_SPEECH_HELPER\`. Packaging the Swift helper and downloadable speech model assets belongs to the macOS Runtime Pack work.

The engine is reported as unsupported outside macOS.

### Remote OpenAI-compatible transcription

Execution: HTTP multipart.

YUZUKI resolves only Provider models whose effective capabilities contain \`speech.asr\`.

The adapter posts to:

\`\`\`text
<provider.base_url>/audio/transcriptions
\`\`\`

It first requests segment timestamps with \`verbose_json\`. If the compatible endpoint rejects that representation with a request-validation error, YUZUKI retries plain JSON **without checking vendor or model names**.

Plain-text JSON responses become one whole-track Segment when the media duration is known. This is explicit lower-fidelity timing, not fake sentence alignment.

## Model evaluation (2026-10)

Same 120 s Japanese ASMR clip, RTX 3050 Ti Laptop (4 GB), faster-whisper 1.2.1, `compute_type=int8_float16`:

| Model | Wall time | Segments | Verdict |
|---|---|---|---|
| `large-v3-turbo` | 15 s | 29 | Accurate text and timings, ~2.0/4 GB VRAM. **Default.** |
| whisper.cpp `small` (old default) | 42 s | 33 | Baseline; turbo is ~2.8x faster. Removed (ADR-019). |
| `anime-whisper` (`quantumcookie/anime-whisper-ct2`) | 16 s | 4 | ~30 s blocks, end timestamps only a few hundred ms after start, repeated hallucination. Not offered in the model list. |
| `Fun-ASR-MLT-Nano-2512` | — | — | Not tested: no timestamp output yet (model card roadmap item unchecked), needs PyTorch + `funasr>=1.4.1` + `trust_remote_code`, 1.97 GB checkpoint. Blocked for subtitles. |

The worker reads the app-decoded 16 kHz mono WAV with `wave` + numpy instead of PyAV: PyAV 19 breaks faster-whisper 1.2.1 (`metadata_errors` argument).

Runtime packs: the spike (embeddable CPython 3.12.0 running the worker from the hash-locked set, ~2.4 GB installed, dominated by cuDNN and cuBLAS) became two packs: `runtime.faster-whisper` (CPU BASE, 97,115,053-byte archive) and the optional `runtime.faster-whisper.cuda` (NVIDIA GPU libraries, 1,372,642,304-byte archive). Sizes, hashes, device selection and publication status: [Runtime bootstrap](06-runtime-bootstrap.md).

## Runtime separation

PR #7 itself installs nothing: Python, CTranslate2 and FunASR come from runtime packs (PR #11/#12, [Runtime bootstrap](06-runtime-bootstrap.md)).

\`\`\`text
PR #7
ASR contract + adapters + UI + result validation

PR #11
Runtime/model discovery + download + verification + activation
\`\`\`

Normal users must not need environment variables in release builds. The environment hooks in PR #7 are developer/runtime-pack seams only.

## Provider Registry relationship

All ASR reuses the same Provider Registry:

\`\`\`text
resolve("speech.asr")
  ↓
Provider Model inventory
  ↓
enabled + available model
  ↓
remote ASR adapter
\`\`\`

Local ASR engines expose the same capability but use a local runtime instead of a Provider transport.

## Canonical replacement behavior

A successful ASR run replaces the Track's canonical source transcript.

Each new Segment records:

\`\`\`text
transcript_provenance = asr:<provider>:<model>
asr_provenance        = same value
reviewed              = false
dirty_translation     = true
dirty_tts             = true
dirty_mix             = true
dirty_subtitle        = true
\`\`\`

An engine failure or invalid result does not erase the existing transcript.

## Validation

ASR results must:

- use contract version 1;
- contain at least one Segment;
- use non-negative timing;
- have \`end_ms > start_ms\`;
- be ordered by start time;
- contain non-empty text.

## Japanese smoke fixture

\`fixtures/asr/ja-smoke.json\` keeps Japanese text in the contract test path so future refactors cannot accidentally assume ASCII/English-only content.

## Deferred

- background Job progress/cancel/retry UI;
- VAD pipeline selection;
- transcript refinement;
- forced alignment;
- word-level editing;
- Runtime Pack installation;
- model download UX;
- video audio extraction.

These remain review-sized follow-up work.
