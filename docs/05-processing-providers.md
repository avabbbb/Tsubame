# Processing Providers and Models

## Architecture rule

> No feature may depend directly on a concrete model name or provider.

Resolve processing through:

```text
Feature
  ↓
Capability
  ↓
Processing Profile
  ↓
Provider
  ↓
Model
  ↓
Runtime / Transport
```

## Capabilities

Initial capability vocabulary:

- `audio.segment`
- `speech.asr`
- `transcript.refine`
- `speech.align`
- `text.translate`
- `speech.tts`
- `speech.voice_clone`
- `audio.separate`
- `speaker.diarize`
- `audio.mix`

## Provider vs model

A **Provider** owns:
- authentication;
- endpoint/base URL;
- model discovery;
- transport/protocol;
- health checks;
- rate/error normalization.

A **Model** declares:
- capabilities;
- languages;
- local/remote execution;
- feature flags such as timestamps, word timestamps, streaming, diarization or voice clone;
- runtime requirements.

## ASR Providers

PR #7 implements the shared ASR result contract and internal adapters described in [Modular ASR Engines](16-modular-asr.md). PR #8 unifies those adapters under the same Provider Registry in [Unified Provider Execution](17-unified-provider-execution.md).

### Initial Providers

The architecture should accommodate at least:

- whisper.cpp as a system-managed local Provider;
- Faster-Whisper / CTranslate2;
- SenseVoice / FunASR;
- Apple SpeechAnalyzer/SpeechTranscriber on supported macOS;
- Alibaba Model Studio speech models;
- OpenAI or compatible transcription endpoints;
- future Japanese-specialized engines.

Whisper is one Provider/model family, not the definition of transcription.

## Transcript refinement

ASR output may optionally pass through a refinement stage before human review.

Typical profile:

```text
Audio
 → VAD / acoustic segmentation
 → ASR
 → Transcript Refiner
 → Alignment
 → Human Review
 → Translation
```

This is important for ASMR where whispering, breath sounds, elongated speech, names and niche vocabulary can challenge a single ASR model.

## BYOK model discovery

PR #6 implements the Provider Registry described in [Provider / Model Registry](15-provider-registry.md).

After a user connects a user-configured Provider:

1. validate credentials;
2. call provider model-list API when available;
3. merge provider metadata with Tsubame's model catalog;
4. optionally run lightweight capability probes;
5. expose unknown models with explicit “unverified capability” state;
6. allow manual override for advanced users.

Never infer all capabilities from model-name string matching.

The registry may use a centralized convenience catalog for recognized families, but runtime feature code only consumes resolved capability metadata. Unknown models remain explicit and can be manually classified.

## OpenAI-compatible provider

A generic provider configuration includes:

```text
Name
Base URL
API key secret reference
Optional organization/project/workspace
Model discovery mode
```

This enables compatible gateways/self-hosted services without creating a new settings UI for each vendor.

## Local and remote execution coexist

Provider execution mode is explicit:

- `remote_api` — paid/hosted API;
- `local_server` — localhost/LAN API such as LM Studio;
- `local_runtime` — Tsubame-managed worker/CLI runtime;
- `native_os` — OS-provided model/runtime such as Apple Speech.

Local provider examples:

- Faster-Whisper runtime pack;
- SenseVoice/FunASR runtime pack;
- IndexTTS runtime pack;
- Apple Speech native provider;
- future ONNX/CoreML/CUDA workers.

Each local runtime advertises Provider + Model capabilities to the same Registry. Features never branch on online/offline mode.

## Processing profiles

Profiles hide complexity from normal users.

Example:

```yaml
name: Japanese · Best Quality
asr:
  provider: faster-whisper
  model: large-v3
refine:
  provider: qwen-byok
  model: auto
align:
  provider: qwen-forced-aligner
translate:
  provider: openai-compatible
  model: auto
tts:
  provider: indextts
  model: 2.5
```

Profiles can be cloned/edited but execution records the exact resolved providers/models/versions used.

## Failure rule

If the selected provider/model is missing, unsupported or unavailable, Tsubame shows the failure and explicit alternatives. It must not silently switch to another model.
