# workers/

External execution engines live here behind versioned contracts.

Workers never own canonical product state and never write directly to Tsubame SQLite.

## ASR

`workers/asr/` contains worker adapters for ecosystems that are better isolated from the Rust Desktop:

- `faster_whisper_worker.py`
- `funasr_worker.py`

They read one JSON request from stdin and write one ASR contract result to stdout.

Runtime packs will supply the Python interpreter, packages and model assets. These scripts do not perform product-level environment management.

Other ASR execution boundaries:

- whisper.cpp → native CLI adapter in Rust;
- Apple SpeechAnalyzer → native macOS helper boundary;
- remote providers → HTTP transport in Rust.

See `docs/16-modular-asr.md`.

## Future workers

- ASMR-Dubber processing worker;
- IndexTTS runtime;
- additional ASR/TTS/refinement engines.

The Desktop remains the canonical state owner.
