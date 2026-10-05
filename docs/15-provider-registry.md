# Provider / Model Registry

Tsubame separates connection/authentication from model capabilities.

```text
Feature
  asks for a capability
        ↓
Capability Resolver
        ↓
ModelDescriptor
        ↓
ProviderConfig
        ↓
transport / local runtime
```

A feature must never branch on a vendor or model ID.

## ProviderConfig

A Provider owns connection-level concerns:

- stable provider ID;
- human-readable name;
- provider kind;
- inference base URL;
- model-discovery URL;
- auth mode;
- `secret_ref`;
- enabled state;
- last refresh/error state.

The inference URL and discovery URL are separate on purpose.

OpenAI exposes model discovery at `GET /v1/models`.

Alibaba Cloud Model Studio exposes a dedicated model-list API at `GET /api/v1/models`, while OpenAI-compatible inference uses `/compatible-mode/v1`. Therefore Tsubame cannot assume every compatible provider discovers models at `{base_url}/models`.

For providers where that convention is valid, an empty discovery URL falls back to `{base_url}/models`.

## BYOK secret policy

Raw API keys are not stored in SQLite.

SQLite contains only a non-secret reference:

```text
tsubame/provider/<provider-id>/api-key
```

The Desktop resolves that reference through the operating-system credential store.

Current Rust implementation uses the cross-platform `keyring` abstraction:

- macOS → Keychain Services;
- Windows → Windows Credential Manager;
- Unix-like desktops → native/Secret Service credential backend.

Provider list/model APIs never return a raw API key to React.

Deleting a provider also requests deletion of its stored credential.

## ModelDescriptor

A discovered model contains:

- provider ID;
- model ID;
- display name;
- owner/vendor metadata when advertised;
- availability;
- discovered/catalog capabilities;
- optional manual capability override;
- effective capabilities;
- capability source;
- last-seen timestamp.

A model disappearing from a refresh is retained but marked unavailable. This keeps historical configuration inspectable without pretending the credential can still access it.

## Capability vocabulary

Initial vocabulary:

- `text.generate`
- `text.translate`
- `transcript.refine`
- `speech.asr`
- `speech.tts`
- `speech.align`
- `audio.separate`
- `embedding.text`
- `image.generate`

This vocabulary is intentionally broader than PR #6's execution surface. PR #6 is registry/discovery only.

## Capability resolution

Resolution order:

1. explicit manual override;
2. provider-advertised / Tsubame catalog metadata;
3. unknown.

Unknown is a valid state.

Tsubame may centrally classify well-known model families for convenience, but execution code must only query the resulting capability set.

For example, the ASR workflow may ask:

```text
resolve("speech.asr")
```

It must not do:

```text
if model_id contains "whisper" ...
```

## Model discovery

The generic OpenAI-compatible transport sends:

```http
GET <model_list_url>
Authorization: Bearer <secret>
```

Authentication may be disabled for local endpoints.

The response parser supports common list shapes such as:

```json
{"data":[{"id":"model-id"}]}
```

and nested model arrays used by compatible providers.

The parser extracts model identity/metadata but does not log the credential or response authorization material.

## Connection test vs refresh

**Test** validates authentication/connectivity and parses the model list without mutating the model inventory.

**Refresh models** performs the same discovery and persists the visible inventory.

Successful discovery proves only that the model is visible to the credential. It does not prove that Tsubame has an execution adapter for every advertised capability.

## Presets

PR #6 ships editable presets for:

- OpenAI;
- Alibaba Cloud Model Studio (Singapore defaults);
- custom OpenAI-compatible provider;
- local OpenAI-compatible endpoint.

Presets are convenience defaults, not special branches in feature code.

Alibaba's workspace-specific domains can replace the preset URLs without changing the provider/model contracts.

## Security boundary

The following are forbidden:

- raw keys in SQLite;
- raw keys in logs;
- raw keys in model/provider JSON returned to UI/CLI/MCP;
- vendor-specific business logic in feature surfaces;
- silently assigning unknown models a capability required for paid or destructive work.

## Follow-up

PR #7 will consume this registry for modular ASR engines.

It will add actual execution adapters for Faster-Whisper, whisper.cpp, SenseVoice/FunASR, Apple Speech and remote ASR providers. Those adapters must resolve through capability/provider contracts established here.
