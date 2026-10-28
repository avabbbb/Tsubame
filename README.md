# YUZUKI

> Local-first ASMR library, sentence-aware player and AI dubbing workbench for desktop.

YUZUKI turns a folder of ASMR / voice works into a study-and-dub workbench: import a work, transcribe it into sentences, translate, listen sentence by sentence, look words up offline, edit timing, regenerate dubs and export. One local app, one library, one segment timeline, one job system.

Status: active development (React + Tauri 2 + Rust + SQLite, clean-room codebase). Desktop installers are built by CI; heavy ASR/TTS runtimes are installed on demand.

## Highlights

| Area | What you get |
| --- | --- |
| Library | Import files or whole folders (recursive; hidden, symlink and non-media files skipped), search and sort, playback position remembered |
| Sentence playback | Click a sentence to seek, auto-pause at sentence end, auto-advance and list loop |
| Listening practice | Blind-listening mode hides translations; hold H to peek |
| Mini subtitle window | Always-on-top floating subtitles with optional click-through lock |
| Shortcuts | Rebindable local shortcuts, opt-in global hotkeys, conflict detection |
| Offline dictionary | Select a word in the sentence list to look it up in JMdict (JA to EN) or ECDICT (EN to ZH), imported by you |
| ASR | Modular providers (Faster-Whisper, SenseVoice, Apple Speech, cloud) chosen by capability |
| Refine and translate | BYOK LLM refinement and translation via OpenAI-compatible and other providers |
| Dubbing | Per-segment TTS, single-sentence regenerate, drift and push indicators, mix and export |
| Runtime packs | On-demand downloads with progress, sha256 verification and resumable partial files |
| Agent control | `yuzuki` CLI and `yuzuki-mcp` over a local control endpoint and Operation Registry |
| Updates | Signed in-app updater; installs only after confirmation |

## Install

Download the latest installer from GitHub Releases (Windows x64 NSIS, macOS Apple Silicon and Intel DMG). Development builds are available as workflow artifacts and are not notarized yet.

On first use, open Settings to add provider keys (BYOK) and install only the runtime packs your processing profile needs.

## Quick start

1. Import a work: the + tab button for files, or import a folder.
2. Transcribe in the ASR panel, optionally refine, then translate.
3. Listen: click sentences, toggle auto-pause, blind mode (hold H), or open the mini subtitle window.
4. Look up words: select text in the sentence list (needs an imported dictionary).
5. Dub: generate or regenerate segment dubs, check drift badges, export subtitles or mixed audio.

Full user guide: [22-product-guide](docs/22-product-guide.md).

## Offline dictionary

YUZUKI does not bundle dictionary data. In Settings, Offline dictionary:

- JMdict: import a [jmdict-simplified](https://github.com/scriptin/jmdict-simplified) JSON file. Data is CC BY-SA 4.0 from the [EDRDG](https://www.edrdg.org/edrdg/licence.html).
- ECDICT: import the CSV from [skywind3000/ECDICT](https://github.com/skywind3000/ECDICT) (MIT).

Data stays local in `dictionary.db` under the app data folder; attribution is shown next to each imported source.

## Develop

Requirements: Node.js, Rust stable and the Tauri 2 platform prerequisites.

```bash
cd app
npm install
npm run tauri dev      # run the desktop app
npm run build          # type-check and build the frontend

cargo fmt --all
cargo clippy -p tsubame --all-targets --all-features --locked -- -D warnings
cargo test -p tsubame --all-features --locked
cargo test -p yuzuki-cli
node scripts/eval/run.mjs --fast --quality   # ASR / translation eval harness
```

## Architecture

```text
Feature -> Capability -> Processing Profile -> Provider -> Model -> Runtime / Transport

Local agent -> $yuzuki Skill -> CLI | MCP -> Local RPC -> YUZUKI Desktop -> Operation Registry
```

Principles: desktop first, segment first, capability first, BYOK for cloud AI, modular on-demand runtimes, agent-operable through stable interfaces, no silent fallback. Details: [00-vision](docs/00-vision.md), [02-architecture](docs/02-architecture.md).

## Documentation

| Topic | Docs |
| --- | --- |
| Product and user guide | [22-product-guide](docs/22-product-guide.md), [01-product](docs/01-product.md), [BASELINE.zh-CN](docs/BASELINE.zh-CN.md) |
| Architecture and domain | [02-architecture](docs/02-architecture.md), [04-domain-model](docs/04-domain-model.md), [DECISIONS](docs/DECISIONS.md) |
| UI and playback | [03-ui-system](docs/03-ui-system.md), [13-playback-workbench](docs/13-playback-workbench.md), [14-segment-editor](docs/14-segment-editor.md) |
| Processing | [05-processing-providers](docs/05-processing-providers.md), [15-provider-registry](docs/15-provider-registry.md), [16-modular-asr](docs/16-modular-asr.md), [17-unified-provider-execution](docs/17-unified-provider-execution.md), [18-transcript-refinement](docs/18-transcript-refinement.md), [19-dubbing-pipeline](docs/19-dubbing-pipeline.md), [20-segment-regenerate](docs/20-segment-regenerate.md) |
| Runtime and release | [06-runtime-bootstrap](docs/06-runtime-bootstrap.md), [21-desktop-releases](docs/21-desktop-releases.md) |
| Agents and sources | [07-agent-control](docs/07-agent-control.md), [08-source-adapters](docs/08-source-adapters.md), [YUZUKI Skill](.agents/skills/yuzuki/SKILL.md) |
| Rights and planning | [09-security-rights](docs/09-security-rights.md), [10-roadmap](docs/10-roadmap.md), [11-clean-room-reset](docs/11-clean-room-reset.md), [12-pr-plan](docs/12-pr-plan.md), [REFERENCES](docs/REFERENCES.md) |

## Releases and updates

Version tags `vX.Y.Z` (matching `app/src-tauri/tauri.conf.json`) run `release-desktop`, which uploads installers, signed updater bundles and `latest.json` to a draft GitHub Release for review before publishing. Signing setup (`YUZUKI_UPDATER_PUBLIC_KEY`, `TAURI_SIGNING_PRIVATE_KEY`, `TAURI_SIGNING_PRIVATE_KEY_PASSWORD`) is mandatory; never commit the private key. See [21-desktop-releases](docs/21-desktop-releases.md).

Installers do not mean runtimes/models are installed; Runtime Bootstrap stays separate.

## Reference projects and licensing

YUZUKI is MIT licensed and stays MIT-compatible by keeping GPL source out of the codebase.

- [ASPlayer](https://github.com/yumili426/ASPlayer): GPL-3.0, reference only for playback and listening UX. Features are reimplemented independently; no source is copied.
- [ASMR-Dubber](https://github.com/EveningStudy/asmr-dubber): MIT; processing ideas may be reused with notices.
- [asmr-downloader / ASMRoner](https://github.com/fireinrain/asmr-downloader): MIT; source and downloader ideas may be reused with notices.
- Dictionary data is never bundled; see Offline dictionary.

## Name

YUZUKI is a personal homage to voice actress Yuzuki Tsubame (柚木つばめ). The project is not affiliated with or endorsed by her and bundles none of her recordings, voice model, likeness, branding or training data. Do not use YUZUKI to train, clone or synthesize a real person's voice without the required rights and permission.

## License

MIT.
