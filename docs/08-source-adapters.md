# Source Adapters and Downloads

## Goal

Yuzuki must not become “an ASMR.one client”.

Content acquisition is a pluggable source capability.

Initial adapters:

- Local files/folders;
- ASMR.one where functional;
- Japanese ASMR;
- future lawful sources.

## SourceAdapter contract

Conceptual methods:

```text
capabilities()
search(query, filters)
get_work(remote_id)
list_assets(remote_id)
resolve_asset(asset_id)
acquire(asset_id, destination)
refresh(record)
```

Adapters normalize remote metadata into Yuzuki domain objects.

## Normalized Work

Typical fields:

```text
source_id
remote_id
rj_code?
title
circle?
voice_actors[]
languages[]
tags[]
cover?
release_date?
assets[]
```

## Asset probing

An adapter should identify available remote/local assets such as:

- audio;
- video;
- subtitle;
- image;
- archive;
- metadata-only entry.

The UI asks “what does this work contain?” rather than hard-coding website directory assumptions.

## Download manager

Shared infrastructure owns:

- queue;
- concurrency;
- retry/backoff;
- resumable Range downloads where supported;
- proxy configuration;
- temporary/staging files;
- hash/integrity validation when metadata exists;
- duplicate detection;
- persisted job status;
- cancellation/recovery.

Adapter code should not implement its own parallel queue UI.

## Rights/access boundary

Adapters operate only on content/resources the user is authorized to access.

Do not implement:
- DRM circumvention;
- paywall bypass;
- credential theft;
- access-control bypass.

Site-specific login/session support, where lawful and supported, must be isolated and secrets stored securely.

## Source durability

Website markup and endpoints are unstable.

Therefore:
- parsing is adapter-contained;
- fixtures/contracts test each adapter;
- adapter failures do not corrupt Library state;
- imported/acquired local assets remain usable even if the source disappears later.
