# 21 — Desktop installers, GitHub Releases and updater

## What this adds

This workflow is intentionally separate from Runtime Bootstrap (model downloads,
Python workers, FFmpeg). It can package the current Desktop UI and included
Rust code, but **cannot** make missing local model runtimes magically work.

- `.github/workflows/build-installers.yml`: PR/main/manual runs build an NSIS
  Windows x64 `.exe`, macOS arm64 `.dmg`, and macOS Intel `.dmg`.
  Outputs are ephemeral Actions artifacts, not published updates.
- `.github/workflows/release-desktop.yml`: tags `vX.Y.Z` build those
  platforms with a Tauri updater signature, upload updater bundles plus
  `latest.json` to a **draft** GitHub Release, and keep it unpublished until
  human verification.
- Desktop: runs a non-blocking update check at startup in signed-release builds.
  The Settings inspector contains **Check now** and (if an update exists)
  **Install**. Installation is opt-in, with download progress and restart.
  Failed checks never interrupt playback or media edits.
- Source of updates: `https://github.com/avabbbb/Yuzuki/releases/latest/download/latest.json`.
  Only **published** Releases are discoverable; mere main commits, draft
  Releases, PR commits, and workflow artifacts are never updater targets.

## Generate the single persistent updater keypair (one-time action)

On a developer machine with Tauri CLI installed, run:

```bash
cd app
npm install
npm run tauri -- signer generate -w ~/.tauri/yuzuki.key
```

Store the **private** key offline. It must never be pasted into an issue,
PR, commit, or chat. GitHub → repository Settings → Secrets and variables →
Actions:

1. Add **secret** `TAURI_SIGNING_PRIVATE_KEY`: the full generated private
   key file contents. Add **secret** `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`
   if the key uses a passphrase.
2. Add **repository variable** `YUZUKI_UPDATER_PUBLIC_KEY`: the complete
   corresponding *public key* from the CLI's generated public key output/file.
   This value is safe to publish, but must match the signing private key.

The release workflow **fails closed** if it lacks the private or public key.
The generated `app/src-tauri/tauri.release.conf.json` file is ignored by Git
and includes only the public key. The private key stays in Actions secrets.
Every release must keep using the same updater keypair or installed clients
cannot authenticate the update. Back up the private key securely.

This is **Tauri updater artifact signing**, distinct from platform vendor
code signing. Development packages are not trusted for auto-updates.

## Release a build

1. Merge the stacked PRs to main and verify Rust/frontend/docs CI.
2. Update the semantic version in `app/src-tauri/tauri.conf.json`,
   `app/src-tauri/Cargo.toml`, and `app/package.json` together.
   E.g. after installing `0.1.0`, the next app update needs `0.1.1`
   or a higher SemVer version.
3. Tag a commit already on main with the identical version (e.g. `v0.1.1`)
   and push the tag. The workflow refuses tags outside main.
4. Review the release job matrix and ensure both `darwin-aarch64` and
   `darwin-x86_64`, Windows NSIS, signatures and `latest.json` are present.
5. Manually **publish the draft Release** only when all artifacts and
   signatures are present and verified. The published release becomes the
   version checked by already installed updater-enabled builds.

Use `Actions → build-installers → Run workflow` to get test installers
without publishing anything.

## OS trust / migration caveats

- **macOS:** CI currently uses ad-hoc code signing (`-`), not Apple
  Developer ID notarization. Users may need to explicitly approve the app
  in macOS Privacy & Security. Configure a real Developer ID certificate
  and notarization *before public production release* for a trusted install.
  This GitHub setup alone does not grant Apple notarization.
- **Windows:** NSIS builds work without paid code signing but may trigger
  SmartScreen reputation warnings. Obtain a code-signing certificate or
  trusted signing solution before wide distribution.
- **Existing local apps:** old pre-updater installations do not gain the
  feature remotely. Install the first updater-capable build once; later
  newer releases can be updated in-app.
- **App identity:** keep `com.avabbbb.tsubame`, the Rust crate name and
  data/cache paths unchanged as in ADR-017. Renaming these would require
  explicit migration to avoid breaking existing installs.
- **Security:** the release should only be tagged from reviewed main.
  Never publish a partially assembled draft or reuse an invalid signing
  key. Publishing a lower version does not trigger downgrade.
- **Functional smoke test:** before trusting updates, install an older
  signed app, publish a newer version using the *same* key, check that
  the app offers the correct version, opt in, and verify user data survives
  the restart on macOS and Windows.

## CI acceptance

- Preview builds produce one Windows installer and both macOS architectures.
- Release workflow rejects a mismatched version/tag or missing signing key.
- Release artifacts include Tauri updater metadata and signatures.
- Preview app does not show an enabled update installation path.
- A signed release checks updates on launch; the user controls installation.
- Runtime/model bootstrap and real desktop ASR/TTS end-to-end testing remain
  separate blockers for an actually usable demo.
