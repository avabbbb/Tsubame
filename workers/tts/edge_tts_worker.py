#!/usr/bin/env python3
"""One-shot Edge TTS worker for the opt-in runtime.edge-tts pack.

CLOUD VOICE: the sentence text is sent to Microsoft's online Edge speech
service and needs internet. Text passes outbound_redact.for_cloud first.

Reads ONE JSON object on stdin:
    {"contract_version": 1, "text": str, "voice": str, "rate": "+0%",
     "pitch": "+0Hz", "volume": "+0%", "output_path": absolute path}
Writes MP3 bytes to output_path and prints ONE JSON line on stdout:
    {"ok": true, "format": "mp3", "bytes": n}
or, with exit code 1:
    {"ok": false, "error": {"code": "network"|"invalid_input"|"synthesis_failed",
                             "message": str, "retryable": bool}}
Honors HTTPS_PROXY / HTTP_PROXY.
"""

from __future__ import annotations

import asyncio
import json
import os
import sys

import aiohttp
import edge_tts

# Embeddable CPython reads python312._pth, which leaves the script directory off sys.path.
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from outbound_redact import for_cloud  # noqa: E402

CONTRACT_VERSION = 1
TIMEOUT_S = 60


class WorkerError(Exception):
    def __init__(self, code: str, message: str, retryable: bool = False):
        super().__init__(message)
        self.code = code
        self.message = message
        self.retryable = retryable


def _network_errors() -> tuple[type[BaseException], ...]:
    errors: list[type[BaseException]] = [aiohttp.ClientError, asyncio.TimeoutError, TimeoutError, ConnectionError]
    ws_error = getattr(getattr(edge_tts, "exceptions", None), "WebSocketError", None)
    if isinstance(ws_error, type):
        errors.append(ws_error)
    return tuple(errors)


def parse_request(raw: bytes) -> dict:
    try:
        req = json.loads(raw.decode("utf-8-sig"))
    except (UnicodeDecodeError, json.JSONDecodeError) as e:
        raise WorkerError("invalid_input", f"request is not JSON: {e}")
    if not isinstance(req, dict):
        raise WorkerError("invalid_input", "request must be a JSON object")
    if req.get("contract_version") != CONTRACT_VERSION:
        raise WorkerError("invalid_input", f"unsupported contract_version {req.get('contract_version')!r}")
    for key in ("text", "voice"):
        value = req.get(key)
        if not isinstance(value, str) or not value.strip():
            raise WorkerError("invalid_input", f"{key} is empty")
    output_path = req.get("output_path")
    if not isinstance(output_path, str) or not os.path.isabs(output_path):
        raise WorkerError("invalid_input", "output_path must be an absolute path")
    for key, default in (("rate", "+0%"), ("pitch", "+0Hz"), ("volume", "+0%")):
        value = req.get(key, default)
        if not isinstance(value, str):
            raise WorkerError("invalid_input", f"{key} must be a string like {default!r}")
        req[key] = value
    return req


async def synth_mp3(text: str, voice: str, rate: str, pitch: str, volume: str) -> bytes:
    proxy = os.environ.get("HTTPS_PROXY") or os.environ.get("HTTP_PROXY")
    communicate = edge_tts.Communicate(text, voice, rate=rate, pitch=pitch, volume=volume, proxy=proxy)
    chunks = []
    async for chunk in communicate.stream():
        if chunk["type"] == "audio":
            chunks.append(chunk["data"])
    if not chunks:
        raise WorkerError("synthesis_failed", "edge-tts returned no audio")
    return b"".join(chunks)


def run(raw: bytes) -> tuple[dict, int]:
    """Result object and exit code for one request."""
    try:
        req = parse_request(raw)
        try:
            audio = asyncio.run(asyncio.wait_for(
                synth_mp3(for_cloud(req["text"]), req["voice"], req["rate"], req["pitch"], req["volume"]),
                TIMEOUT_S,
            ))
        except WorkerError:
            raise
        except _network_errors() as e:
            raise WorkerError("network", f"{type(e).__name__}: {e}", retryable=True)
        except ValueError as e:
            raise WorkerError("invalid_input", str(e))
        except Exception as e:  # noqa: BLE001 - every other edge-tts failure is reported, not raised
            raise WorkerError("synthesis_failed", f"{type(e).__name__}: {e}")
        out = req["output_path"]
        part = out + ".part"
        try:
            with open(part, "wb") as f:
                f.write(audio)
            os.replace(part, out)
        except OSError as e:
            raise WorkerError("synthesis_failed", f"cannot write output: {e}")
        return {"ok": True, "format": "mp3", "bytes": len(audio)}, 0
    except WorkerError as e:
        return {"ok": False, "error": {"code": e.code, "message": e.message, "retryable": e.retryable}}, 1


def main() -> int:
    result, code = run(sys.stdin.buffer.read())
    sys.stdout.write(json.dumps(result) + "\n")
    sys.stdout.flush()
    return code


if __name__ == "__main__":
    sys.exit(main())
