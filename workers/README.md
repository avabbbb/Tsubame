# workers/

External execution engines live here behind versioned contracts.

Initial targets include:

- ASMR-Dubber adapter/worker;
- Faster-Whisper runtime pack;
- SenseVoice/FunASR runtime pack;
- IndexTTS runtime pack.

Workers never own canonical product state and never write directly to Yuzuki SQLite.
