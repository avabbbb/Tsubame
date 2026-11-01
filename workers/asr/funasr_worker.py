#!/usr/bin/env python3
"""YUZUKI FunASR/SenseVoice worker.

Reads one JSON request from stdin and writes one versioned ASR result to stdout.
The runtime pack (runtime.funasr) owns Python, PyTorch (CPU) and FunASR; models
download through ModelScope into MODELSCOPE_CACHE. SenseVoice emits no
timestamps, so FSMN-VAD splits the audio into speech spans and each span is
transcribed on its own: segment times are the VAD boundaries.
"""

from __future__ import annotations

import contextlib
import json
import re
import sys

VAD_MODEL = "fsmn-vad"
SAMPLE_RATE = 16000
# SenseVoice prefixes each span with <|lang|><|EMOTION|><|Event|><|itn|> tags.
# rich_transcription_postprocess maps emotion/event tags to emoji, which then
# leak into subtitles, translation and TTS, so the tags are dropped instead.
SENSEVOICE_TAG = re.compile(r"<\|[^|<>]*\|>")


def clean_text(text: str) -> str:
    return SENSEVOICE_TAG.sub("", text).strip()


def fail(message: str) -> None:
    print(message, file=sys.stderr)
    raise SystemExit(2)


def load_audio(path: str):
    import numpy as np
    import soundfile as sf

    data, rate = sf.read(path, dtype="float32", always_2d=True)
    data = data.mean(axis=1)
    if rate != SAMPLE_RATE:
        import librosa

        data = librosa.resample(data, orig_sr=rate, target_sr=SAMPLE_RATE)
    return np.ascontiguousarray(data, dtype=np.float32)


def vad_spans(vad, audio, duration_ms: int):
    result = vad.generate(input=audio, fs=SAMPLE_RATE)
    first = result[0] if isinstance(result, list) and result else {}
    spans = []
    for item in (first.get("value") or []) if isinstance(first, dict) else []:
        if isinstance(item, (list, tuple)) and len(item) >= 2:
            start, end = max(0, int(item[0])), int(item[1])
            if end < 0:  # open-ended span at the end of the input
                end = duration_ms
            if end > start:
                spans.append((start, end))
    return spans


def main() -> None:
    try:
        request = json.load(sys.stdin)
    except Exception as exc:
        fail(f"invalid worker request: {exc}")

    if request.get("contract_version") != 1:
        fail("unsupported ASR contract version")

    model_id = str(request.get("model_id") or "iic/SenseVoiceSmall")
    language = request.get("language")
    language = language if language not in (None, "", "auto") else None

    # FunASR and ModelScope print banners and progress to stdout; stdout carries
    # only the result JSON, so everything before it goes to stderr.
    with contextlib.redirect_stdout(sys.stderr):
        try:
            from funasr import AutoModel
        except Exception as exc:
            fail(f"FunASR runtime is unavailable: {exc}")

        audio = load_audio(request["media_path"])
        duration_ms = int(len(audio) * 1000 / SAMPLE_RATE)
        vad = AutoModel(model=VAD_MODEL, disable_update=True, device="cpu")
        asr = AutoModel(model=model_id, disable_update=True, device="cpu")
        spans = vad_spans(vad, audio, duration_ms)

        segments = []
        for start, end in spans:
            chunk = audio[start * SAMPLE_RATE // 1000 : end * SAMPLE_RATE // 1000]
            if len(chunk) < SAMPLE_RATE // 10:
                continue
            out = asr.generate(
                input=chunk,
                fs=SAMPLE_RATE,
                language=language or "auto",
                # ITN punctuates zh/en/yue well but splits Japanese and Korean
                # words with spaces and commas, so those stay raw.
                use_itn=language not in ("ja", "ko"),
            )
            first = out[0] if isinstance(out, list) and out else out
            if not isinstance(first, dict):
                fail("FunASR returned an unsupported result shape")
            text = clean_text(str(first.get("text") or ""))
            if text:
                segments.append(
                    {"start_ms": start, "end_ms": end, "text": text, "confidence": None}
                )

    if not segments:
        fail("FunASR returned no transcript text")

    output = {
        "contract_version": 1,
        "adapter_id": "funasr-sensevoice",
        "model_id": model_id,
        "language": language,
        "segments": segments,
        "notes": [
            f"device=cpu vad={VAD_MODEL} spans={len(spans)}; segment times are VAD speech boundaries"
        ],
    }
    # Explicit UTF-8: a piped sys.stdout on Windows uses the ANSI code page.
    sys.stdout.buffer.write(json.dumps(output, ensure_ascii=False).encode("utf-8"))
    sys.stdout.buffer.flush()


if __name__ == "__main__":
    main()
