# Source Adapters and Downloads

## Goal

YUZUKI must not become “an ASMR.one client”.

Content acquisition is a pluggable source capability.

Adapters:

- Local files/folders (implemented);
- DLsite public RJ metadata (implemented; read-only, no login);
- DLsite account: sign-in, purchased library and official downloads of purchased works (implemented, see "DLsite account" below);
- future lawful sources.

ASMR.one and Japanese ASMR (japaneseasmr.com) are out of scope: they redistribute paid DLsite works without the rights holders' authorization (docs/research/2026-10-sources.md), which the Rights/access boundary below rules out. This covers search, metadata, subtitles and downloads, including through a generic scraper.

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

Adapters normalize remote metadata into YUZUKI domain objects.

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

## Implementation (PR #14)

Code: app/src-tauri/src/sources/ (mod.rs registry and cache tables, local.rs, dlsite.rs public metadata, dlsite_auth.rs account session, purchases and owned downloads, download.rs queue and worker). Adapters: `local` (files/folders; remote_id is an absolute path) and `dlsite` (public RJ metadata; no login).

Operations (UI, CLI and MCP share them):

- source.list: adapters with their capabilities.
- source.search {source_id, query, limit?}: normalized works.
- source.get_work {source_id, remote_id, refresh?}: the cached work unless refresh, else asks the adapter and caches it (source_works, source_assets).
- download.enqueue {source_id, remote_id, asset_ids?, dest_dir, import?}: queues the work's assets (all when asset_ids is omitted) under dest_dir, keeping only plain name components; returns the rows and the download job.
- download.list {status?}, download.cancel {id}, download.retry {id}.
- dlsite.session_status {}: {signed_in}; read-only, no network.
- dlsite.logout {}: deletes the stored session from the OS keyring.
- dlsite.library {}: the signed-in account's purchases as [{rj_code, purchased_at}] (play.dlsite.com content/sales); metadata comes from source.get_work.

Queue: the `downloads` table. A (url, dest) pair is one row. One `download` job (media_id 0) drains the queue sequentially on its own SQLite connection, so a long body never holds the product DB lock; enqueue/retry start it when none is live. Bodies stage at `<dest>.part` and resume with Range; size and sha256 are checked when known; the file is renamed into place only after verification. Retryable failures back off and retry; cancel stops a queued row at once and a running row at its next chunk; job.cancel stops the whole drain. On startup rows a crash left running return to the queue with their .part kept. Proxy: HTTP(S)_PROXY / NO_PROXY through runtime::HttpFetcher. Landed audio/video with import (default true) goes through the same import path as media.import_folder.

Not yet: parallel workers.

### DLsite account

Sign-in (Tauri commands dlsite_login_open / dlsite_login_finish, Discover rail): DLsite's own login page opens in a separate dlsite-login window with its own webview data directory, limited to dlsite.com hosts. YUZUKI never injects, reads or stores the password. When the window reaches a signed-in play.dlsite.com page, only the DLsite cookies (play, www, login hosts) are joined into one Cookie header, verified with an authenticated content/count request and stored in the OS keyring under tsubame/dlsite/session; the window's browsing data is then wiped and it closes. The session is never written to SQLite, download rows, errors or logs. A rejected or lapsed session (401/403, or a login page instead of JSON or a file) fails with unauthorized and asks the user to sign in again.

Owned downloads: download.enqueue with source_id dlsite and asset_ids containing dlsite-owned-archive first checks that the RJ code is among the account's purchases (else not_owned, nothing queued), then queues DLsite's official download page for that work (<RJ>.zip under dest_dir). The row stores only that URL. At fetch time the worker adds the keyring session to that single request; reqwest drops the Cookie when DLsite redirects to another host, and a login page instead of a file fails non-retryably as unauthorized. Omitted asset_ids still mean the public assets only. Multi-part archives of very large works are a follow-up.


## Name normalization

Voice-actor identity must preserve the original Japanese display name and keep romanized names as aliases rather than replacing the canonical source name.

Examples observed in the target ecosystem:

- `大山チロル` → common Western-order display `Chiroru Oyama`; literal romanization also appears as `Ooyama Chiroru`.
- `秋山はるる` → `Haruru Akiyama`.
- `篠守ゆきこ` → `Yukiko Shinomori` (Japanese-order romanization: `Shinomori Yukiko`).

Adapters should not use romanized display strings as identity keys. Prefer source IDs / RJ metadata / canonical Japanese names plus aliases.
