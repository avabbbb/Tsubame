#!/usr/bin/env python3
"""Tsubame Faster-Whisper worker.

Reads one JSON request from stdin and writes one versioned ASR result to stdout.
The runtime pack owns Python/CTranslate2/model installation; this file owns only
the Tsubame worker contract.
"""

from __future__ import annotations

import json
import math
import sys


def fail(message: str) -> None:
    print(message, file=sys.stderr)
    raise SystemExit(2)


def main() -> None:
    try:
        request = json.load(sys.stdin)
    except Exception as exc:
        fail(f"invalid worker request: {exc}")

    if request.get("contract_version") != 1:
        fail("unsupported ASR contract version")

    try:
        from faster_whisper import WhisperModel
    except Exception as exc:
        fail(f"faster-whisper runtime is unavailable: {exc}")

    model_id = str(request.get("model_id") or "large-v3")
    language = request.get("language")
    if language in (None, "", "auto"):
        language = None

    # Runtime packs may inject device/compute defaults through environment later.
    model = WhisperModel(model_id)
    segments, info = model.transcribe(
        request["media_path"],
        language=language,
        initial_prompt=request.get("prompt") or None,
        vad_filter=True,
    )

    rows = []
    for segment in segments:
        text = (segment.text or "").strip()
        if not text:
            continue
        avg_logprob = getattr(segment, "avg_logprob", None)
        confidence = None
        if avg_logprob is not None:
            confidence = max(0.0, min(1.0, math.exp(float(avg_logprob))))
        rows.append(
            {
                "start_ms": round(float(segment.start) * 1000),
                "end_ms": round(float(segment.end) * 1000),
                "text": text,
                "confidence": confidence,
            }
        )

    result = {
        "contract_version": 1,
        "engine_id": "faster-whisper",
        "model_id": model_id,
        "language": getattr(info, "language", None),
        "segments": rows,
        "notes": [],
    }
    json.dump(result, sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()
