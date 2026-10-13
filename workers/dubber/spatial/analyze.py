"""Track-level spatial cue analysis for ``audio.spatialize`` (op ``analyze``).

Portions adapted from ASMR-Dubber (MIT), Copyright (c) 2026 ASMR Dubber
contributors, https://github.com/EveningStudy/asmr-dubber commit 2faea94f,
file ``src/asmr_dubber/spatial_cues.py`` (``analyze``, ``smooth``, ``_fill``,
``Cues.summary``). Original notes kept below.

    Reference-guided binaural cue transfer, not a neural renderer or 3-D tracker.
    Assumes a single dominant speaker in two coherent microphone channels.
    Positive ITD means the right ear receives the sound later (source toward left).
    Broadband/time-smoothed features deliberately avoid copying Japanese phonemes.

Tsubame changes (numerically equivalent per frame, see tests/test_golden.py):
- runs once per media over the whole reference and streams it in frame batches
  from a memory-mapped WAV instead of loading it;
- per-frame FFT work is vectorised across a batch of frames;
- keeps the raw per-frame estimates, the activity mask and the ITD reliability
  mask next to Dubber's smoothed tracks so the renderer can *hold* positions on
  low-confidence frames instead of interpolating through noise;
- adds a position-reliability mask for ILD (see ``position_reliable``);
- parameters are explicit and versioned, and the result is content-addressed.
"""

from __future__ import annotations

import hashlib
import json
import math
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

import numpy as np

from . import ANALYZER_VERSION
from ._wav import WavReader

DEFAULT_PARAMS: dict = {
    "hop_ms": 20,
    "frame_ms": 85,  # rounded up to the next power-of-two sample count
    "bands": 28,
    "band_lo_hz": 100.0,
    "band_hi_hz": 18000.0,  # also capped at 0.44 * sample rate
    "band_halfwidth_ratio": 1.22,
    "ild_clip_db": 24.0,
    "itd_lo_hz": 180.0,
    "itd_hi_hz": 6500.0,  # also capped at 0.4 * sample rate
    "itd_max_ms": 0.9,
    "itd_reliable_coherence": 0.15,
    "activity_floor_db": -85.0,
    "activity_below_p90_db": 30.0,
    "color_clip_db": 6.0,
    # ILD frames whose broadband |ILD| reaches this are position-reliable even with
    # low coherence: a hard-panned source leaves one ear (almost) silent, so the
    # L/R correlation is meaningless there while the level difference is decisive.
    "ild_hardpan_db": 12.0,
    "smoothing": {"itd_median": 5, "itd": 7, "ild": 9, "level": 19, "color": 19},
}

BATCH_FRAMES = 128


class AnalysisError(ValueError):
    """Reference cannot be analysed; ``code`` is the protocol error slug."""

    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


def resolve_params(params: dict | None) -> dict:
    """Merge caller params over defaults; reject unknown keys and bad values."""
    out = json.loads(json.dumps(DEFAULT_PARAMS))
    for key, value in (params or {}).items():
        if key not in out:
            raise AnalysisError("invalid_input", f"unknown analysis param: {key}")
        if key == "smoothing":
            if not isinstance(value, dict):
                raise AnalysisError("invalid_input", "params.smoothing must be an object")
            for k, v in value.items():
                if k not in out["smoothing"]:
                    raise AnalysisError("invalid_input", f"unknown smoothing key: {k}")
                if not isinstance(v, int) or isinstance(v, bool) or v < 1 or v > 201:
                    raise AnalysisError("invalid_input", f"smoothing.{k} must be int 1..201")
                out["smoothing"][k] = v
            continue
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
            raise AnalysisError("invalid_input", f"param {key} must be a finite number")
        # Normalise numeric type to the default's so 20 and 20.0 key identically.
        if isinstance(DEFAULT_PARAMS[key], int):
            if float(value) != int(value):
                raise AnalysisError("invalid_input", f"param {key} must be an integer")
            value = int(value)
        else:
            value = float(value)
        if key == "bands" and not 4 <= value <= 64:
            raise AnalysisError("invalid_input", "bands must be int 4..64")
        if key in ("hop_ms", "frame_ms") and not 1 <= value <= 200:
            raise AnalysisError("invalid_input", f"{key} must be 1..200")
        out[key] = value
    return out


def canonical_json(value) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False)


def spatial_track_key(
    reference_sha256: str, reference_kind: str, params: dict | None,
    analyzer_version: str = ANALYZER_VERSION,
) -> str:
    """Contract key ``sha256(reference_sha256 ‖ reference_kind ‖ params ‖ analyzer_version)``.

    ``‖`` is realised as a canonical JSON array (sorted keys, no whitespace,
    UTF-8) of a domain tag and the four fields, with params fully resolved
    against defaults, so equal effective params always give the same key.
    """
    payload = ["tsubame.spatial_track.v1", reference_sha256.lower(), reference_kind,
               resolve_params(params), analyzer_version]
    return hashlib.sha256(canonical_json(payload).encode("utf-8")).hexdigest()


def smooth(x: np.ndarray, width: int) -> np.ndarray:
    """Centred moving average with edge padding (Dubber ``spatial_cues.smooth``)."""
    width = max(1, int(width) | 1)
    if width == 1 or len(x) == 0:
        return x.copy()
    pad = width // 2
    padded = np.pad(x, [(pad, pad)] + [(0, 0)] * (x.ndim - 1), mode="edge")
    acc = np.concatenate([np.zeros_like(padded[:1]), np.cumsum(padded, axis=0)], axis=0)
    return (acc[width:] - acc[:-width]) / width


def median_filter(x: np.ndarray, width: int) -> np.ndarray:
    width = max(1, int(width) | 1)
    if width == 1:
        return x.copy()
    pad = width // 2
    return np.median(
        np.lib.stride_tricks.sliding_window_view(np.pad(x, (pad, pad), mode="edge"), width),
        axis=1,
    )


def interp_fill(values: np.ndarray, valid: np.ndarray) -> np.ndarray:
    """Linear interpolation across invalid frames (Dubber ``_fill``)."""
    if not valid.any():
        return np.zeros_like(values)
    positions = np.arange(len(values))
    if values.ndim == 1:
        return np.interp(positions, positions[valid], values[valid])
    return np.column_stack(
        [np.interp(positions, positions[valid], column[valid]) for column in values.T]
    )


def rms(x: np.ndarray) -> float:
    return float(np.sqrt(np.mean(np.square(x, dtype=np.float64)))) if x.size else 0.0


@dataclass
class SpatialTrack:
    """Per-frame cues for one reference. Smoothed fields match Dubber's ``Cues``."""

    sample_rate: int
    hop: int
    frame_size: int
    times: np.ndarray  # frame centres, seconds
    band_hz: np.ndarray
    ild_db: np.ndarray  # (F, bands) smoothed, Dubber-equivalent
    itd_seconds: np.ndarray  # (F,) smoothed, Dubber-equivalent
    level_db: np.ndarray  # (F,) smoothed
    color_db: np.ndarray  # (F, bands)
    confidence: np.ndarray  # (F,) raw coherence after lag alignment, >= 0
    active: np.ndarray  # (F,) bool
    itd_reliable: np.ndarray  # (F,) bool: active & confidence >= threshold
    position_reliable: np.ndarray  # (F,) bool: ILD usable as a position
    ild_raw_db: np.ndarray  # (F, bands) unsmoothed
    itd_raw_seconds: np.ndarray  # (F,) unsmoothed GCC-PHAT estimate
    params: dict = field(default_factory=dict)
    meta: dict = field(default_factory=dict)

    def summary(self) -> dict:
        """Dubber ``Cues.summary`` plus Tsubame activity/reliability fractions."""
        itd = self.itd_seconds
        return {
            "method": "reference-guided spectral ILD + GCC-PHAT ITD + relative level/color",
            "itd_convention": "positive = right ear delayed",
            "ild_convention": "positive = left louder",
            "itd_ms_range": (itd[[np.argmin(itd), np.argmax(itd)]] * 1000).tolist(),
            "median_ild_db_range": [
                float(x) for x in np.percentile(np.median(self.ild_db, axis=1), [5, 95])
            ],
            "reliable_itd_fraction": float(np.mean(self.confidence >= self.params["itd_reliable_coherence"])),
            "active_fraction": float(np.mean(self.active)),
            "position_reliable_fraction": float(np.mean(self.position_reliable)),
            "frames": int(len(self.times)),
            "hop_seconds": self.hop / self.sample_rate,
            "duration_seconds": float(self.meta.get("duration_seconds", 0.0)),
            "distance": "relative loudness/color proxy only; no metric distance or room recovery",
        }

    # -- persistence -------------------------------------------------------
    def save(self, path: Path) -> None:
        meta = dict(self.meta, sample_rate=self.sample_rate, hop=self.hop,
                    frame_size=self.frame_size, params=self.params)
        with open(path, "wb") as f:
            np.savez(
                f,
                meta_json=np.array(canonical_json(meta)),
                times=self.times.astype(np.float64),
                band_hz=self.band_hz.astype(np.float64),
                ild_db=self.ild_db.astype(np.float32),
                itd_seconds=self.itd_seconds.astype(np.float64),
                level_db=self.level_db.astype(np.float32),
                color_db=self.color_db.astype(np.float32),
                confidence=self.confidence.astype(np.float32),
                active=self.active.astype(np.bool_),
                itd_reliable=self.itd_reliable.astype(np.bool_),
                position_reliable=self.position_reliable.astype(np.bool_),
                ild_raw_db=self.ild_raw_db.astype(np.float32),
                itd_raw_seconds=self.itd_raw_seconds.astype(np.float64),
            )

    @classmethod
    def load(cls, path: str | Path) -> "SpatialTrack":
        with np.load(path, allow_pickle=False) as z:
            meta = json.loads(str(z["meta_json"]))
            if meta.get("analyzer_version") != ANALYZER_VERSION:
                raise AnalysisError(
                    "track_version_mismatch",
                    f"spatial track built by {meta.get('analyzer_version')}, need {ANALYZER_VERSION}",
                )
            arrays = {k: np.asarray(z[k]) for k in z.files if k != "meta_json"}
        f64 = {k: v.astype(np.float64) if v.dtype.kind == "f" else v for k, v in arrays.items()}
        return cls(
            sample_rate=int(meta.pop("sample_rate")), hop=int(meta.pop("hop")),
            frame_size=int(meta.pop("frame_size")), params=meta.pop("params"), meta=meta, **f64,
        )


def _frame_size(rate: int, frame_ms: float) -> int:
    return 1 << int(np.ceil(np.log2(rate * frame_ms / 1000)))


def analyze_reader(
    reader: WavReader,
    params: dict | None = None,
    *,
    progress: Callable[[str, int, int], None] | None = None,
    check_cancel: Callable[[], None] | None = None,
) -> SpatialTrack:
    p = resolve_params(params)
    info = reader.info
    rate = info.sample_rate
    if info.channels != 2:
        raise AnalysisError("reference_not_stereo",
                            f"spatial analysis needs a stereo reference, got {info.channels} channel(s)")
    if info.frames < rate // 10:
        raise AnalysisError("reference_too_short", "reference shorter than 0.1 s")
    hop = round(rate * p["hop_ms"] / 1000)
    size = _frame_size(rate, p["frame_ms"])
    half = size // 2
    centers = np.arange(0, info.frames, hop)
    window = np.hanning(size)
    win_norm = np.sqrt(np.mean(window**2))
    hz = np.fft.rfftfreq(size, 1 / rate)
    bands = np.geomspace(p["band_lo_hz"], min(p["band_hi_hz"], rate * 0.44), int(p["bands"]))
    w = p["band_halfwidth_ratio"]
    masks = np.array([(hz >= f / w) & (hz <= f * w) for f in bands], dtype=np.float64)
    counts = masks.sum(axis=1)
    if (counts == 0).any():
        raise AnalysisError("invalid_input", "a band has no FFT bins at this sample rate")
    band_matrix = masks / counts[:, None]  # mean over band bins
    low_band = (hz >= p["itd_lo_hz"]) & (hz <= min(p["itd_hi_hz"], rate * 0.4))
    maxlag = round(rate * p["itd_max_ms"] / 1000)
    clip_db = p["ild_clip_db"]

    n = len(centers)
    ild = np.empty((n, len(bands)))
    spectra = np.empty((n, len(bands)))
    levels = np.empty(n)
    delays = np.empty(n)
    confidence = np.empty(n)
    total = (n + BATCH_FRAMES - 1) // BATCH_FRAMES
    offsets = np.arange(size)
    for bi, f0 in enumerate(range(0, n, BATCH_FRAMES)):
        if check_cancel:
            check_cancel()
        f1 = min(n, f0 + BATCH_FRAMES)
        c = centers[f0:f1]
        # Frame k covers samples [c_k - half, c_k + half) of the unpadded signal.
        span = reader.read(int(c[0]) - half, int(c[-1]) + half)
        frames = span[(c - c[0])[:, None] + offsets[None, :]]  # (B, size, 2)
        frames = frames * window[None, :, None]
        spectrum = np.fft.rfft(frames, axis=1)  # (B, bins, 2)
        power = np.abs(spectrum) ** 2
        # Regularize bins that contain very little speech energy.
        floor = np.maximum(power.max(axis=(1, 2)) * 1e-7, 1e-14)  # (B,)
        band_power = (np.swapaxes(power, 1, 2) @ band_matrix.T).swapaxes(1, 2) + floor[:, None, None]
        ild[f0:f1] = np.clip(10 * np.log10(band_power[:, :, 0] / band_power[:, :, 1]), -clip_db, clip_db)
        spectra[f0:f1] = 10 * np.log10(band_power.mean(axis=2))
        frame_rms = np.sqrt(np.mean(frames**2, axis=(1, 2)))
        levels[f0:f1] = 20 * np.log10(np.maximum(frame_rms / win_norm, 1e-9))
        # R * conj(L) yields positive lag when R is delayed.
        cross = spectrum[:, :, 1] * np.conj(spectrum[:, :, 0])
        cross = np.where(low_band[None, :], cross / np.maximum(np.abs(cross), floor[:, None]), 0)
        corr = np.fft.irfft(cross, n=size, axis=1)
        search = np.concatenate([corr[:, -maxlag:], corr[:, : maxlag + 1]], axis=1)
        peak = np.argmax(search, axis=1)
        frac = np.zeros(len(c))
        inner = (peak > 0) & (peak < search.shape[1] - 1)
        rows = np.flatnonzero(inner)
        if len(rows):
            pk = peak[rows]
            a = search[rows, pk - 1]
            b = search[rows, pk]
            cc = search[rows, pk + 1]
            den = a - 2 * b + cc
            ok = np.abs(den) > 1e-12
            frac[rows[ok]] = np.clip(0.5 * (a[ok] - cc[ok]) / den[ok], -0.5, 0.5)
        delay = (peak - maxlag + frac) / rate
        delays[f0:f1] = delay
        lags = np.round(delay * rate).astype(int)
        coh = np.empty(len(c))
        for lag in np.unique(lags):
            sel = np.flatnonzero(lags == lag)
            fr = frames[sel]
            if lag >= 0:
                left, right = fr[:, : size - lag, 0], fr[:, lag:, 1]
            else:
                left, right = fr[:, -lag:, 0], fr[:, : size + lag, 1]
            num = np.sum(left * right, axis=1)
            den = np.maximum(np.linalg.norm(left, axis=1) * np.linalg.norm(right, axis=1), 1e-12)
            coh[sel] = num / den
        confidence[f0:f1] = np.maximum(coh, 0)
        if progress:
            progress("analyze", bi + 1, total)

    if not np.isfinite(levels).all() or not np.isfinite(ild).all():
        raise AnalysisError("reference_invalid", "reference contains non-finite samples")
    active = levels > max(p["activity_floor_db"], float(np.percentile(levels, 90)) - p["activity_below_p90_db"])
    if not active.any() or float(levels.max()) <= -170:
        raise AnalysisError("reference_silent", "reference level too low for spatial analysis")
    reliable = active & (confidence >= p["itd_reliable_coherence"])
    s = p["smoothing"]
    delay_f = median_filter(interp_fill(delays, reliable), s["itd_median"])
    ild_s = smooth(interp_fill(ild, active), s["ild"])
    level_s = smooth(interp_fill(levels, active), s["level"])
    spec = smooth(interp_fill(spectra, active), s["color"])
    # Remove broadband level before computing gentle, relative coloration.
    spec -= np.mean(spec, axis=1, keepdims=True)
    color = np.clip(spec - np.median(spec[active], axis=0), -p["color_clip_db"], p["color_clip_db"])
    broadband_ild = np.median(ild, axis=1)
    position_reliable = active & (
        (confidence >= p["itd_reliable_coherence"]) | (np.abs(broadband_ild) >= p["ild_hardpan_db"])
    )
    return SpatialTrack(
        sample_rate=rate, hop=hop, frame_size=size, times=centers / rate, band_hz=bands,
        ild_db=ild_s, itd_seconds=smooth(delay_f, s["itd"]), level_db=level_s, color_db=color,
        confidence=confidence, active=active, itd_reliable=reliable,
        position_reliable=position_reliable, ild_raw_db=ild, itd_raw_seconds=delays,
        params=p,
        meta={"analyzer_version": ANALYZER_VERSION, "duration_seconds": info.frames / rate,
              "reference_frames": info.frames},
    )


def analyze_file(path: str | Path, params: dict | None = None, **kw) -> SpatialTrack:
    with WavReader(path) as reader:
        return analyze_reader(reader, params, **kw)


def analyze_array(audio: np.ndarray, rate: int, params: dict | None = None) -> SpatialTrack:
    """Analyse an in-memory (frames, 2) array (tests and golden comparisons)."""

    class _ArrayReader:
        def __init__(self, a):
            from ._wav import WavInfo

            self._a = np.asarray(a, dtype=np.float64)
            ch = self._a.shape[1] if self._a.ndim == 2 else 1
            self.info = WavInfo(Path("<array>"), rate, ch, len(self._a), 3, 64, 0)

        def read(self, start, stop):
            out = np.zeros((stop - start, self.info.channels))
            a, b = max(0, start), min(len(self._a), stop)
            if a < b:
                out[a - start : b - start] = self._a[a:b].reshape(b - a, -1)
            return out

    return analyze_reader(_ArrayReader(audio), params)  # type: ignore[arg-type]
