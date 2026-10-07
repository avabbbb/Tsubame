# Unified Provider Execution

Tsubame uses **one Provider Registry** for both local and remote inference.

There is no product-level “ASR engine registry” beside the Provider Registry.

## Canonical chain

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
Adapter
  ↓
Transport / Runtime
```

A Provider describes **where/how execution happens**. A Model describes **what can execute there**.

## Execution modes

### remote_api

Paid/BYOK or hosted services.

Examples:
- OpenAI-compatible APIs;
- Alibaba Model Studio;
- future hosted ASR/TTS/translation providers.

Typical concerns:
- credential;
- rate limits;
- endpoint;
- model discovery;
- billing/network errors.

### local_server

A model runs locally, but Tsubame talks to it through a localhost/LAN API.

Examples:
- LM Studio;
- Ollama/OpenAI-compatible servers;
- llama.cpp server;
- other self-hosted gateways.

This is intentionally the same Provider UX as remote APIs. Authentication may be `none`.

A local LLM can therefore satisfy:

```text
text.generate
text.translate
transcript.refine
```

without any special translation UI.

### local_runtime

Tsubame owns/launches a local runtime or sidecar.

Examples:
- Faster-Whisper;
- SenseVoice/FunASR;
- whisper.cpp CLI/runtime;
- future IndexTTS worker.

Runtime/model installation is delegated to Runtime Packs.

### native_os

The operating system provides the model/runtime.

Initial example:
- Apple SpeechAnalyzer / SpeechTranscriber.

## Provider vs adapter

These are not synonyms.

Example:

```text
Provider
  local.faster-whisper

Model
  large-v3

Adapter
  faster-whisper worker adapter

Execution
  local_runtime
```

The UI, Processing Profile and provenance record **Provider + Model**.

The adapter is an internal implementation detail and may be replaced without changing project data.

## ASR

ASR now resolves:

```text
resolve("speech.asr")
  ↓
CapabilityTarget[]
  ↓
Provider + Model
  ↓
provider.kind selects internal adapter
  ↓
AsrResult candidates
  ↓
Desktop validation
  ↓
canonical Segment[]
```

Initial system-managed local Providers:

- `local.faster-whisper`
- `local.sensevoice`
- `local.whisper-cpp`
- `local.apple-speech`

User-configured OpenAI-compatible Providers can also expose `speech.asr`.

## Translation/refinement

Exactly the same abstraction applies to text processing:

```text
resolve("text.translate")
```

may return:

- a paid remote model;
- Alibaba/OpenAI-compatible model;
- a local LM Studio model;
- another localhost model server;
- a future Tsubame-managed local LLM runtime.

No translation feature should contain an “online vs local” branch. It selects a Provider + Model that satisfies the requested capability.

## Local server model discovery

Local API servers may expose OpenAI-compatible model discovery. For example, LM Studio exposes `GET /v1/models` and OpenAI-compatible inference on localhost.

Therefore local servers reuse the same model inventory and manual capability override mechanism as hosted endpoints.

## Runtime readiness

System-managed Providers are visible even when not ready.

Their availability can be:

- `ready`;
- `runtime-required`;
- `unsupported-platform`;
- `error`.

This lets the UI show “install/download required” instead of hiding the capability.

PR #11 Runtime Bootstrap will replace development environment hooks with verified app-managed Runtime/Model Packs.

## Provenance

Canonical provenance records Provider + Model:

```text
asr:local.faster-whisper:large-v3
asr:my-openai-provider:gpt-4o-transcribe
translate:lm-studio-local:qwen3-local
```

It does not encode the current adapter implementation.
