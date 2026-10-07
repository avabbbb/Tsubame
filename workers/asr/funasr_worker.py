#!/usr/bin/env python3
"""Tsubame FunASR/SenseVoice worker."""

from __future__ import annotations

import json
import sys


def fail(message: str) -> None:
    print(message, file=sys.stderr)
    raise SystemExit(2)


def normalize_timestamps(raw, text: str, duration_ms: int):
    if isinstance(raw, list) and raw:
        rows = []
        # FunASR timestamps are commonly [start_ms, end_ms] pairs.
        for item in raw:
            if not isinstance(item, (list, tuple)) or len(item) < 2:
                continue
            start, end = int(item[0]), int(item[1])
            if end > start:
                rows.append((start, end))
        if rows:
            # Some checkpoints expose word/character timestamps without sentence
            # grouping. Preserve the whole text as a single honest span rather
            # than inventing sentence boundaries.
            return 0, max(end for _, end in rows)
    return 0, max(1, int(duration_ms))


def main() -> None:
    try:
        request = json.load(sys.stdin)
    except Exception as exc:
        fail(f"invalid worker request: {exc}")

    if request.get("contract_version") != 1:
        fail("unsupported ASR contract version")

    try:
        from funasr import AutoModel
    except Exception as exc:
        fail(f"FunASR runtime is unavailable: {exc}")

    model_id = str(request.get("model_id") or "iic/SenseVoiceSmall")
    model = AutoModel(model=model_id)
    kwargs = {"input": request["media_path"]}
    language = request.get("language")
    if language not in (None, "", "auto"):
        kwargs["language"] = language

    result = model.generate(**kwargs)
    first = result[0] if isinstance(result, list) and result else result
    if not isinstance(first, dict):
        fail("FunASR returned an unsupported result shape")

    text = str(first.get("text") or "").strip()
    if not text:
        fail("FunASR returned no transcript text")

    start_ms, end_ms = normalize_timestamps(
        first.get("timestamp"),
        text,
        int(request.get("media_duration_ms") or 0),
    )
    output = {
        "contract_version": 1,
        "adapter_id": "funasr-sensevoice",
        "model_id": model_id,
        "language": language if language not in (None, "", "auto") else None,
        "segments": [
            {
                "start_ms": start_ms,
                "end_ms": end_ms,
                "text": text,
                "confidence": None,
            }
        ],
        "notes": [
            "SenseVoice/FunASR checkpoint output is normalized without inventing sentence boundaries."
        ],
    }
    json.dump(output, sys.stdout, ensure_ascii=False)


if __name__ == "__main__":
    main()
