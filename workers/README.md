# workers/

External execution engines live here behind versioned contracts.

Workers never own canonical product state and never write directly to YUZUKI SQLite.

## ASR

`workers/asr/` contains worker adapters for ecosystems that are better isolated from the Rust Desktop:

- `faster_whisper_worker.py`
- `funasr_worker.py`

They read one JSON request from stdin and write one ASR contract result to stdout.

Runtime packs will supply the Python interpreter, packages and model assets. These scripts do not perform product-level environment management.

Other ASR execution boundaries:

- Apple SpeechAnalyzer → native macOS helper boundary;
- remote providers → HTTP transport in Rust.

See `docs/16-modular-asr.md`.

## Dubbing workers (protocol v1)

`workers/dubber/` holds long-lived workers that speak **worker protocol v1**:
JSON Lines over stdio with a `hello` capability handshake, streamed
`progress`/`log`, `result`/`error`, cooperative `cancel` (2 s grace, then kill)
and `shutdown`. The Rust host is `app/src-tauri/src/worker.rs`; job state lives
in the `jobs` table (`app/src-tauri/src/jobs.rs`). See
[workers/dubber/README.md](dubber/README.md) and
[docs/19-dubbing-pipeline.md](../docs/19-dubbing-pipeline.md).

- `tsubame_worker/` — the Python SDK every protocol v1 worker uses;
- `echo_worker.py` — test worker for the host's conformance tests;
- `spatial/` — `audio.spatialize` (ported from ASMR-Dubber's spatial analysis).

The ASR workers above still use the older one-shot JSON contract.

## Future workers

- local TTS runtimes (e.g. IndexTTS) as protocol v1 workers for `speech.tts`;
- additional ASR/TTS/refinement engines, migrating to protocol v1.

The Desktop remains the canonical state owner.
