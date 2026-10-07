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

Provider adapters do **not** own Tsubame state and do not write SQLite.

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
Tsubame Desktop
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

SenseVoice/FunASR can produce rich speech-understanding output. Tsubame normalizes only what it can represent honestly in the ASR contract.

If a checkpoint/runtime exposes timestamps without reliable sentence grouping, the worker does **not invent sentence boundaries**. It returns a whole-span Segment and records a note. Refinement/alignment is a later stage.

Development runtime hook: \`TSUBAME_FUNASR_PYTHON\`.

Default model hint: \`iic/SenseVoiceSmall\`.

### whisper.cpp

Provider: `local.whisper-cpp` · execution: `local_runtime` · adapter: native CLI.

Reference:
- https://github.com/ggml-org/whisper.cpp

The adapter requests JSON output and consumes segment offsets/text.

Development/runtime hooks:
- \`TSUBAME_WHISPER_CPP_BIN\`
- \`TSUBAME_WHISPER_CPP_MODEL\`

This PR accepts 16-bit WAV input for whisper.cpp. General media extraction/normalization belongs to Runtime Bootstrap so the ASR layer does not acquire a second FFmpeg lifecycle.

### Apple SpeechAnalyzer

Provider: `local.apple-speech` · execution: `native_os` · adapter: macOS native helper.

Reference:
- https://developer.apple.com/videos/play/wwdc2025/277/

Apple's SpeechAnalyzer/SpeechTranscriber is treated as a local \`speech.asr\` engine, not an OpenAI-compatible provider.

The Rust adapter expects a versioned helper that reads the same JSON request/result contract. During development it is discovered via \`TSUBAME_APPLE_SPEECH_HELPER\`. Packaging the Swift helper and downloadable speech model assets belongs to the macOS Runtime Pack work.

The engine is reported as unsupported outside macOS.

### Remote OpenAI-compatible transcription

Execution: HTTP multipart.

Tsubame resolves only Provider models whose effective capabilities contain \`speech.asr\`.

The adapter posts to:

\`\`\`text
<provider.base_url>/audio/transcriptions
\`\`\`

It first requests segment timestamps with \`verbose_json\`. If the compatible endpoint rejects that representation with a request-validation error, Tsubame retries plain JSON **without checking vendor or model names**.

Plain-text JSON responses become one whole-track Segment when the media duration is known. This is explicit lower-fidelity timing, not fake sentence alignment.

## Runtime separation

This PR intentionally does not install Python, CTranslate2, FunASR, whisper.cpp binaries or model checkpoints.

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
