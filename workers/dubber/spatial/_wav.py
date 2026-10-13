"""Minimal RIFF/RF64 WAV reader/writer on numpy + stdlib only.

The stdlib ``wave`` module rejects IEEE-float WAV, which is what TTS engines and
analysis copies commonly produce, so this module parses the container itself.
Reads are memory-mapped so long references are streamed, never loaded whole.
Supported: PCM 8/16/24/32-bit, IEEE float 32/64-bit, WAVE_FORMAT_EXTENSIBLE.
"""

from __future__ import annotations

import os
import struct
from dataclasses import dataclass
from pathlib import Path

import numpy as np

_PCM = 1
_FLOAT = 3
_EXTENSIBLE = 0xFFFE


class WavError(ValueError):
    """The file is not a WAV this module can read."""


@dataclass
class WavInfo:
    path: Path
    sample_rate: int
    channels: int
    frames: int
    fmt: int
    bits: int
    data_offset: int

    @property
    def duration_s(self) -> float:
        return self.frames / self.sample_rate


def probe(path: str | os.PathLike) -> WavInfo:
    path = Path(path)
    with path.open("rb") as f:
        head = f.read(12)
        if len(head) < 12 or head[8:12] != b"WAVE" or head[:4] not in (b"RIFF", b"RF64"):
            raise WavError(f"not a RIFF/RF64 WAVE file: {path.name}")
        rf64 = head[:4] == b"RF64"
        ds64_data = None
        fmt = None
        size = path.stat().st_size
        while True:
            chunk = f.read(8)
            if len(chunk) < 8:
                break
            cid, clen = chunk[:4], struct.unpack("<I", chunk[4:])[0]
            start = f.tell()
            if cid == b"ds64":
                body = f.read(clen)
                ds64_data = struct.unpack("<Q", body[8:16])[0]
            elif cid == b"fmt ":
                body = f.read(clen)
                tag, channels, rate, _, _, bits = struct.unpack("<HHIIHH", body[:16])
                if tag == _EXTENSIBLE and len(body) >= 40:
                    tag = struct.unpack("<H", body[24:26])[0]
                fmt = (tag, channels, rate, bits)
            elif cid == b"data":
                if fmt is None:
                    raise WavError("data chunk before fmt chunk")
                length = ds64_data if (rf64 and clen == 0xFFFFFFFF and ds64_data) else clen
                length = min(length, size - start)
                tag, channels, rate, bits = fmt
                if tag not in (_PCM, _FLOAT):
                    raise WavError(f"unsupported WAV sample format tag {tag}")
                if tag == _PCM and bits not in (8, 16, 24, 32):
                    raise WavError(f"unsupported PCM bit depth {bits}")
                if tag == _FLOAT and bits not in (32, 64):
                    raise WavError(f"unsupported float bit depth {bits}")
                if channels < 1 or rate < 1:
                    raise WavError("invalid channel count or sample rate")
                frames = length // (channels * bits // 8)
                return WavInfo(path, rate, channels, frames, tag, bits, start)
            f.seek(start + clen + (clen & 1))
    raise WavError(f"no data chunk in {path.name}")


class WavReader:
    """Random-access float64 reader over a memory-mapped WAV data chunk."""

    def __init__(self, path: str | os.PathLike):
        self.info = probe(path)
        i = self.info
        width = i.bits // 8
        if i.frames == 0:
            self._raw = np.zeros((0, i.channels * width), dtype=np.uint8)
        else:
            self._raw = np.memmap(
                i.path, dtype=np.uint8, mode="r", offset=i.data_offset,
                shape=(i.frames, i.channels * width),
            )

    def read(self, start: int, stop: int) -> np.ndarray:
        """Samples [start, stop) as float64 (frames, channels); zero outside the file."""
        i = self.info
        start, stop = int(start), int(stop)
        out = np.zeros((max(0, stop - start), i.channels), dtype=np.float64)
        a, b = max(0, start), min(i.frames, stop)
        if a < b:
            out[a - start : b - start] = self._decode(np.asarray(self._raw[a:b]))
        return out

    def _decode(self, raw: np.ndarray) -> np.ndarray:
        i = self.info
        n = len(raw)
        flat = np.ascontiguousarray(raw).reshape(-1)
        if i.fmt == _FLOAT:
            data = flat.view("<f4" if i.bits == 32 else "<f8").astype(np.float64)
        elif i.bits == 8:
            data = (flat.astype(np.float64) - 128.0) / 128.0
        elif i.bits == 16:
            data = flat.view("<i2").astype(np.float64) / 32768.0
        elif i.bits == 32:
            data = flat.view("<i4").astype(np.float64) / 2147483648.0
        else:  # 24-bit little-endian
            b = flat.reshape(-1, 3).astype(np.int32)
            v = b[:, 0] | (b[:, 1] << 8) | (b[:, 2] << 16)
            v = np.where(v & 0x800000, v - 0x1000000, v)
            data = v.astype(np.float64) / 8388608.0
        return data.reshape(n, i.channels)

    def close(self) -> None:
        mm = getattr(self._raw, "_mmap", None)
        self._raw = None
        if mm is not None:
            mm.close()

    def __enter__(self) -> "WavReader":
        return self

    def __exit__(self, *exc) -> None:
        self.close()


def read(path: str | os.PathLike) -> tuple[np.ndarray, int]:
    with WavReader(path) as r:
        return r.read(0, r.info.frames), r.info.sample_rate


def write_float32(path: str | os.PathLike, audio: np.ndarray, rate: int) -> None:
    """Write (frames, channels) audio as IEEE float32 WAV (RIFF, < 4 GiB)."""
    audio = np.asarray(audio, dtype="<f4")
    if audio.ndim == 1:
        audio = audio[:, None]
    frames, channels = audio.shape
    data_len = frames * channels * 4
    if data_len + 50 > 0xFFFFFFFF:
        raise WavError("clip too large for RIFF WAV")
    fmt = struct.pack("<HHIIHH", _FLOAT, channels, rate, rate * channels * 4, channels * 4, 32)
    fmt += struct.pack("<H", 0)  # cbSize
    fact = struct.pack("<I", frames)
    body = (
        b"WAVE" + b"fmt " + struct.pack("<I", len(fmt)) + fmt
        + b"fact" + struct.pack("<I", 4) + fact
        + b"data" + struct.pack("<I", data_len)
    )
    with open(path, "wb") as f:
        f.write(b"RIFF" + struct.pack("<I", len(body) + data_len))  # size after this field
        f.write(body)
        f.write(audio.tobytes())
