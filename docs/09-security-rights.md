# Security, Privacy and Rights

## Local-first defaults

User media, edits, playback state and job history stay local unless a selected remote provider/source requires network access.

## Secrets

API keys/tokens must not be stored in:
- SQLite;
- logs;
- prompts;
- crash reports;
- project manifests;
- command history;
- git.

Store secrets in the OS credential store and persist only an opaque `secret_ref`.

## Remote provider transparency

Before remote inference, Yuzuki can show:
- provider;
- model;
- data class being uploaded;
- estimated/known cost when available;
- whether audio, transcript, translation or reference audio leaves the device.

No hidden remote fallback.

## Runtime supply chain

Downloaded runtimes/models use:
- explicit manifest;
- trusted source/mirror list;
- checksum verification;
- staging before activation;
- recorded version/provenance.

## Real-person voices

Voice synthesis/voice-clone features must distinguish:
- synthetic/publicly licensed voices;
- user-owned/authorized reference voices;
- real-person voices with unclear/no permission.

Yuzuki must not ship unauthorized real-person voice models or recordings.

### Project name and Tsubame Yuzuki

The name **Yuzuki** is a personal homage to voice actress **Tsubame Yuzuki / 柚木つばめ**.

This repository is not affiliated with or endorsed by her.

Her official website states that AI learning/use of her voice is prohibited. Therefore Yuzuki must not:
- bundle her voice recordings as model/reference assets;
- train or distribute a voice model based on her recordings;
- present a built-in “Tsubame Yuzuki voice”;
- imply that the project is official or approved by her.

References:
- https://yuzuki-tsubame.com/
- https://yuzuki-tsubame.com/job-request/

The same principle applies to other real people: model/reference use requires the rights/permission appropriate to that use.

## Source content

Source adapters only access content the user is entitled to access. Yuzuki does not provide DRM/paywall/access-control bypass.

## Agent safety

Local agents:
- use revision-checked mutations;
- receive redacted provider config;
- never receive raw API keys unless a provider action absolutely requires an isolated secret handoff;
- should plan large downloads/paid inference before execution.
