#!/usr/bin/env python3
"""Loopback OpenAI-compatible speech endpoint backed by edge-tts.

YUZUKI's speech.tts adapter speaks OpenAI POST {base}/audio/speech and expects
WAV bytes back. edge-tts streams MP3 from Microsoft's online voices, so this shim
translates one request into one edge-tts call and transcodes to PCM16 WAV with
ffmpeg. Development aid for the simultaneous-interpretation run (docs/23); not a
product runtime pack.

Usage: python edge_tts_shim.py [--port 5810]
Proxy: honours HTTPS_PROXY (edge-tts needs it behind the GFW).
Request fields: input (text), voice (edge voice id, default zh-CN-XiaoyiNeural),
speed (0.5-2.0, mapped to edge rate), pitch (e.g. "+5Hz"), volume (e.g. "+0%").
"""

from __future__ import annotations

import argparse
import asyncio
import json
import os
import struct
import subprocess
import sys
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import edge_tts

from outbound_redact import for_cloud

DEFAULT_VOICE = "zh-CN-XiaoyiNeural"
PROXY = os.environ.get("HTTPS_PROXY") or os.environ.get("HTTP_PROXY")
FFMPEG = os.environ.get("FFMPEG", "ffmpeg")


def rate_from_speed(speed) -> str:
    try:
        value = float(speed)
    except (TypeError, ValueError):
        return "+0%"
    value = min(max(value, 0.5), 2.0)
    pct = round((value - 1.0) * 100)
    return f"{pct:+d}%"


async def synth_mp3(text: str, voice: str, rate: str, pitch: str, volume: str) -> bytes:
    communicate = edge_tts.Communicate(text, voice, rate=rate, pitch=pitch, volume=volume, proxy=PROXY)
    chunks = []
    async for chunk in communicate.stream():
        if chunk["type"] == "audio":
            chunks.append(chunk["data"])
    if not chunks:
        raise RuntimeError("edge-tts returned no audio")
    return b"".join(chunks)


def fix_wav_sizes(wav: bytes) -> bytes:
    """ffmpeg cannot seek back on a pipe, so RIFF/data sizes stay 0xFFFFFFFF; rewrite them."""
    if len(wav) < 12 or wav[:4] != b"RIFF" or wav[8:12] != b"WAVE":
        return wav
    buf = bytearray(wav)
    struct.pack_into("<I", buf, 4, len(buf) - 8)
    pos = 12
    while pos + 8 <= len(buf):
        cid = bytes(buf[pos:pos + 4])
        size = struct.unpack_from("<I", buf, pos + 4)[0]
        if cid == b"data":
            struct.pack_into("<I", buf, pos + 4, len(buf) - pos - 8)
            break
        pos += 8 + size + (size & 1)
    return bytes(buf)


def mp3_to_wav(mp3: bytes) -> bytes:
    proc = subprocess.run(
        [FFMPEG, "-hide_banner", "-loglevel", "error", "-i", "pipe:0", "-ac", "1", "-ar", "24000",
         "-c:a", "pcm_s16le", "-f", "wav", "pipe:1"],
        input=mp3, capture_output=True, check=False,
    )
    if proc.returncode != 0 or len(proc.stdout) < 64:
        raise RuntimeError("ffmpeg transcode failed: " + proc.stderr.decode("utf-8", "replace")[:300])
    return fix_wav_sizes(proc.stdout)


class Handler(BaseHTTPRequestHandler):
    server_version = "yuzuki-edge-tts-shim/1"

    def _send(self, status: int, body: bytes, content_type: str) -> None:
        self.send_response(status)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _error(self, status: int, message: str) -> None:
        self._send(status, json.dumps({"error": {"message": message}}).encode(), "application/json")

    def do_GET(self):  # health check
        if self.path.rstrip("/") in ("/health", "/v1/models"):
            self._send(200, json.dumps({"data": [{"id": "edge-tts"}]}).encode(), "application/json")
        else:
            self._error(404, "not found")

    def do_POST(self):
        if not self.path.rstrip("/").endswith("/audio/speech"):
            return self._error(404, "only /v1/audio/speech is served")
        try:
            length = int(self.headers.get("Content-Length", "0"))
            req = json.loads(self.rfile.read(length) or b"{}")
        except (ValueError, json.JSONDecodeError) as exc:
            return self._error(400, f"invalid JSON: {exc}")
        # Only sentence text leaves the device (docs/09), even when a caller bypasses the Desktop.
        text = for_cloud(str(req.get("input", "")).strip())
        if not text:
            return self._error(400, "input is empty")
        voice = str(req.get("voice") or DEFAULT_VOICE)
        try:
            mp3 = asyncio.run(synth_mp3(
                text, voice, rate_from_speed(req.get("speed", 1.0)),
                str(req.get("pitch", "+0Hz")), str(req.get("volume", "+0%")),
            ))
            wav = mp3_to_wav(mp3)
        except Exception as exc:  # surfaced to YUZUKI as a retryable 502
            return self._error(502, f"{type(exc).__name__}: {exc}")
        self._send(200, wav, "audio/wav")

    def log_message(self, fmt, *args):
        sys.stderr.write("shim: " + (fmt % args) + "\n")


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port", type=int, default=5810)
    args = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", args.port), Handler)
    print(f"edge-tts shim listening on 127.0.0.1:{args.port} proxy={'set' if PROXY else 'none'}", flush=True)
    server.serve_forever()


if __name__ == "__main__":
    main()
