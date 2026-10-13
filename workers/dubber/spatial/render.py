"""Per-clip spatial follow for ``audio.spatialize`` (op ``render``).

Portions adapted from ASMR-Dubber (MIT), Copyright (c) 2026 ASMR Dubber
contributors, https://github.com/EveningStudy/asmr-dubber commit 2faea94f,
files ``src/asmr_dubber/spatial_rtf.py`` (``stft``, ``istft``, ``filters``,
``render_rtf``) and ``src/asmr_dubber/experimental_mix.py``
(``spatialize_clip``, ``_short_reference_fallback``). Original notes:

    Experimental complex relative-transfer baseline (not a pretrained neural model).
    Single-source, locally stationary spatial covariance -> dominant eigenvector.
    Retain frequency-dependent interaural phase, unlike a scalar-delay renderer.
    No noise-only covariance is available: this is NOT covariance whitening or DP-RTF.
    Only differential filtering is identifiable; common room response is not recovered.

Tsubame changes:
- cues come from the cached per-media ``SpatialTrack`` instead of re-analysing
  every block; the reference STFT is still computed for the window only;
- two time mappings: ``source_trajectory`` (sentence path stretched to the clip
  length, Dubber's behaviour) and ``concurrent`` (reference position at the
  dub's play time, falling back to the end of the source trajectory when the
  original is inactive there);
- low-confidence frames *hold* the last reliable position and release slowly
  (one-pole, ``HOLD_RELEASE_MS``) instead of interpolating through noise;
- each block's reference window carries ``CONTEXT_MS`` of extra reference on
  both sides so covariance smoothing is continuous across sentence edges;
- the reference is resampled to the clip rate in numpy (no soxr);
- the clip is never time-stretched: output frames == input frames;
- loudness is left to ``audio.mix``: blocks are RMS-matched back to the dry
  clip and no per-line peak ceiling is applied.
"""

from __future__ import annotations

import hashlib
import math
import os
import uuid
from dataclasses import dataclass, field
from pathlib import Path
from typing import Callable

import numpy as np

from . import RENDERER_VERSION
from ._wav import WavReader, read as wav_read, write_float32
from .analyze import SpatialTrack, canonical_json, median_filter, rms, smooth

MODES = ("source_trajectory", "concurrent")
DEFAULT_STRENGTHS = {"rtf": 1.0, "level": 0.7, "color": 0.35}
CONTEXT_MS = 200
HOLD_RELEASE_MS = 150
HOLD_MEDIAN_FRAMES = 5
BLOCK_SECONDS = 10.0
BLOCK_PAD_SECONDS = 0.2
FADE_SECONDS = 0.008
CONCURRENT_MIN_ACTIVE = 0.25
SOURCE_END_SPAN_SECONDS = 1.0


class RenderError(ValueError):
    def __init__(self, code: str, message: str):
        super().__init__(message)
        self.code = code


# --------------------------------------------------------------------------
# keys and validation


def resolve_strengths(strengths: dict | None) -> dict:
    out = dict(DEFAULT_STRENGTHS)
    for key, value in (strengths or {}).items():
        if key not in out:
            raise RenderError("invalid_input", f"unknown strength: {key}")
        if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(value):
            raise RenderError("invalid_input", f"strength {key} must be a finite number")
        hi = 1.0 if key == "rtf" else 2.0
        if not 0 <= value <= hi:
            raise RenderError("invalid_input", f"strength {key} must be within 0..{hi}")
        out[key] = float(value)
    return out


def key_window(mode: str, window: dict) -> dict:
    """The part of the window that can change the rendered samples."""
    w = {"src_start_ms": int(window["src_start_ms"]), "src_end_ms": int(window["src_end_ms"])}
    if mode == "concurrent":
        w["dub_start_ms"] = int(window["dub_start_ms"])
    return w


def render_key(
    tts_asset_key: str, spatial_track_key: str, mode: str, window: dict,
    strengths: dict | None, renderer_version: str = RENDERER_VERSION,
) -> str:
    """Contract key ``sha256(tts_asset_key ‖ spatial_track_key ‖ mode ‖ window ‖ strengths ‖ renderer_version)``.

    ``‖`` is a canonical JSON array (sorted keys, no whitespace, UTF-8) with a
    domain tag. ``window`` keeps only fields that affect output: in
    ``source_trajectory`` mode ``dub_start_ms`` is excluded, so placement /
    timing edits do not re-render.
    """
    payload = ["tsubame.spatial_render.v1", tts_asset_key, spatial_track_key, mode,
               key_window(mode, window), resolve_strengths(strengths), renderer_version]
    return hashlib.sha256(canonical_json(payload).encode("utf-8")).hexdigest()


# --------------------------------------------------------------------------
# DSP helpers (Dubber spatial_rtf)


def stft(audio: np.ndarray, size: int, hop: int) -> np.ndarray:
    padded = np.pad(audio, ((size // 2, size), (0, 0)))
    n = (len(padded) - size) // hop + 1
    window = np.sqrt(np.hanning(size))
    idx = np.arange(n)[:, None] * hop + np.arange(size)[None, :]
    return np.fft.rfft(padded[idx] * window[None, :, None], axis=1)


def istft(spectra: np.ndarray, size: int, hop: int, length: int) -> np.ndarray:
    window = np.sqrt(np.hanning(size))
    output = np.zeros((hop * (len(spectra) - 1) + size, spectra.shape[-1]))
    weights = np.zeros(len(output))
    for i, frame in enumerate(np.fft.irfft(spectra, n=size, axis=1)):
        start = i * hop
        output[start : start + size] += frame * window[:, None]
        weights[start : start + size] += window**2
    output /= np.maximum(weights[:, None], 1e-12)
    return output[size // 2 : size // 2 + length]


def resample(x: np.ndarray, src: int, dst: int) -> np.ndarray:
    """Band-limited FFT resampling along axis 0 (window-local; edges padded by context)."""
    if src == dst or len(x) == 0:
        return x
    n = len(x)
    m = max(1, round(n * dst / src))
    spec = np.fft.rfft(x, axis=0)
    out = np.zeros((m // 2 + 1,) + x.shape[1:], dtype=complex)
    k = min(len(spec), len(out))
    out[:k] = spec[:k]
    return np.fft.irfft(out, n=m, axis=0) * (m / n)


# --------------------------------------------------------------------------
# position trajectory with hold-and-release


def _hold_release(values: np.ndarray, valid: np.ndarray, history: np.ndarray, releasing: bool,
                  alpha: float, tol: float) -> np.ndarray:
    """Zero-order hold on invalid frames, one-pole release back onto valid ones.

    The held value is the median of the last ``HOLD_MEDIAN_FRAMES`` valid raw
    frames, so a single boundary frame (half voice, half noise) is not frozen in.
    ``history`` seeds that window with valid frames preceding ``values``.
    """
    out = np.empty_like(values)
    recent = [np.asarray(h, dtype=np.float64) for h in history[-HOLD_MEDIAN_FRAMES:]]
    y = np.median(recent, axis=0) if recent else np.asarray(values[0], dtype=np.float64)
    was_valid = not releasing
    for t in range(len(values)):
        if not valid[t]:
            if was_valid and not releasing and recent:
                y = np.median(recent, axis=0)
            releasing = True
        elif releasing:
            y = y + alpha * (values[t] - y)
            if np.max(np.abs(values[t] - y)) <= tol:
                releasing = False
        else:
            y = values[t]
        if valid[t]:
            recent.append(np.asarray(values[t], dtype=np.float64))
            del recent[:-HOLD_MEDIAN_FRAMES]
        was_valid = bool(valid[t])
        out[t] = y
    return out


@dataclass
class Position:
    times: np.ndarray
    itd: np.ndarray
    ild: np.ndarray  # (F, bands)
    held: np.ndarray  # (F,) bool: position not directly reliable (held or released)
    has_position: bool
    band_hz: np.ndarray


def position_trajectory(track: SpatialTrack, t0: float, t1: float,
                        release_ms: float = HOLD_RELEASE_MS) -> Position:
    """Held/released ITD+ILD for frames covering [t0, t1] (absolute seconds)."""
    p = track.params["smoothing"]
    hop_s = track.hop / track.sample_rate
    n = len(track.times)
    rel_frames = max(1.0, release_ms / 1000 / hop_s)
    alpha = 1 - math.exp(-1 / rel_frames)
    margin = max(p["itd_median"], p["itd"], p["ild"]) + int(5 * rel_frames) + 2
    f0 = max(0, int(math.floor(t0 / hop_s)) - margin)
    f1 = min(n, int(math.ceil(t1 / hop_s)) + margin + 1)
    if f1 <= f0:
        f0, f1 = max(0, n - 1), n
    out_lo = max(0, int(math.floor(t0 / hop_s)) - 1)
    out_hi = min(n, int(math.ceil(t1 / hop_s)) + 2)

    def run(values, valid, tol):
        before = np.flatnonzero(valid[:f0])[-HOLD_MEDIAN_FRAMES:]
        if len(before):
            history, releasing = values[before], before[-1] < f0 - 1
        else:
            after = np.flatnonzero(valid[f0:])
            if not len(after):
                return None
            # Before the first reliable frame: hold it backwards (no release).
            history, releasing = values[f0 + after[:1]], True
        return _hold_release(values[f0:f1], valid[f0:f1], history, releasing, alpha, tol)

    itd = run(track.itd_raw_seconds, track.itd_reliable, 1e-6)
    ild = run(track.ild_raw_db, track.position_reliable, 0.1)
    has_position = ild is not None or itd is not None
    if itd is None:
        itd = np.zeros(f1 - f0)
    if ild is None:
        ild = np.zeros((f1 - f0, track.ild_raw_db.shape[1]))
    itd = smooth(median_filter(itd, p["itd_median"]), p["itd"])
    ild = smooth(ild, p["ild"])
    s = slice(out_lo - f0, out_hi - f0)
    return Position(track.times[out_lo:out_hi], itd[s], ild[s],
                    ~track.position_reliable[out_lo:out_hi], has_position, track.band_hz)


# --------------------------------------------------------------------------
# RTF filters (Dubber spatial_rtf.filters with injected cues)


def rtf_filters(reference: np.ndarray, rate: int, ref_t0: float, pos: Position,
                size: int, hop: int, check: Callable[[], None]):
    spec = stft(reference, size, hop)
    times = ref_t0 + np.arange(len(spec)) * hop / rate
    hz = np.fft.rfftfreq(size, 1 / rate)
    itd = np.interp(times, pos.times, pos.itd)
    ll, rr = np.abs(spec[:, :, 0]) ** 2, np.abs(spec[:, :, 1]) ** 2
    # Derotate the estimated bulk ITD before covariance averaging: otherwise
    # motion cancels cross spectra at high frequencies and destroys coherence.
    rotation = np.exp(2j * np.pi * itd[:, None] * hz)
    cross = spec[:, :, 1] * spec[:, :, 0].conj() * rotation
    del spec, rotation
    check()
    time_width = max(3, round(0.16 * rate / hop))
    ll, rr, cross = (smooth(x, time_width) for x in (ll, rr, cross))
    ll, rr, cross = (smooth(x.T, 3).T for x in (ll, rr, cross))
    power = ll + rr
    floor = np.maximum(np.max(power, axis=1, keepdims=True) * 1e-5, 1e-14)
    coherence = np.clip(np.abs(cross) ** 2 / np.maximum(ll * rr, floor**2), 0, 1)
    # Closed-form principal eigenvector ratio v_R/v_L of a Hermitian 2x2 matrix.
    discriminant = np.sqrt((ll - rr) ** 2 + 4 * np.abs(cross) ** 2)
    eigenvalue = (ll + rr + discriminant) / 2
    magnitude = np.sqrt(np.maximum(eigenvalue - ll, floor) / np.maximum(eigenvalue - rr, floor))
    log_ratio = np.clip(20 * np.log10(magnitude), -24, 24)
    del discriminant, eigenvalue, magnitude
    check()
    prior_ild = np.stack([np.interp(times, pos.times, col) for col in pos.ild.T], axis=1)
    prior_ratio = np.stack([-np.interp(hz, pos.band_hz, row) for row in prior_ild])
    reliability = np.clip((coherence - 0.35) / 0.45, 0, 1)
    reliability *= np.clip(power / (floor * 20), 0, 1)
    reliability *= ((hz >= 100) & (hz <= min(18000, rate * 0.44)))[None, :]
    reliability = smooth(smooth(reliability, 5).T, 5).T
    log_ratio = prior_ratio * (1 - reliability) + log_ratio * reliability
    residual = np.unwrap(np.unwrap(np.angle(cross), axis=1), axis=0)
    residual -= 2 * np.pi * np.round(np.median(residual[:, :5], axis=1)[:, None] / (2 * np.pi))
    residual = np.clip(residual, -np.pi, np.pi) * reliability
    residual = smooth(residual, 5)
    total_phase = residual - 2 * np.pi * itd[:, None] * hz
    ratio = 10 ** (log_ratio / 20)
    left_gain = np.sqrt(2 / (1 + ratio**2))
    right_gain = ratio * left_gain
    response = np.stack(
        [left_gain * np.exp(-0.5j * total_phase), right_gain * np.exp(0.5j * total_phase)], axis=-1
    )
    response[:, 0, :] = np.abs(response[:, 0, :])
    response[:, -1, :] = np.abs(response[:, -1, :])
    return response, {
        "mean_rtf_confidence": float(np.mean(reliability)),
        "reliable_bin_fraction": float(np.mean(reliability > 0.5)),
    }


# --------------------------------------------------------------------------
# rendering


@dataclass
class _Ctx:
    track: SpatialTrack
    ref: WavReader
    rate: int
    strengths: dict
    size: int
    hop: int
    check: Callable[[], None]
    report: dict = field(default_factory=dict)


def _level_fallback(mono: np.ndarray, ref: np.ndarray) -> np.ndarray:
    """Dubber ``_short_reference_fallback``: broadband L/R level only, no phase."""
    ratio = 1.0
    if ref.size and rms(ref) >= 1e-7:
        levels = np.sqrt(np.mean(ref.astype(np.float64) ** 2, axis=0))
        ratio = float(np.clip(levels[1] / max(levels[0], 1e-8), 0.25, 4))
    gains = np.array([1, ratio]) * np.sqrt(2 / (1 + ratio**2))
    return mono[:, None] * gains[None, :]


def _render_block(c: _Ctx, mono: np.ndarray, ref_times: np.ndarray, frame_times: np.ndarray):
    """Render ``mono`` whose STFT frame k sits at reference time ``ref_times[k]``.

    ``frame_times`` are reference times per output sample (for fallbacks).
    Returns (stereo, info) where info names any fallback used.
    """
    tr = c.track
    dur = tr.meta["duration_seconds"]
    ctx = CONTEXT_MS / 1000
    lo_t, hi_t = float(ref_times.min()), float(ref_times.max())
    if hi_t - lo_t < SOURCE_END_SPAN_SECONDS / 2:
        # Static or near-static mapping: give the covariance some history.
        lo_t = hi_t - SOURCE_END_SPAN_SECONDS
    r0 = max(0.0, lo_t - ctx)
    r1 = min(dur, hi_t + ctx)
    sr = c.ref.info.sample_rate
    s0, s1 = int(round(r0 * sr)), int(round(r1 * sr))
    ref = c.ref.read(s0, max(s0 + 1, s1))
    if not np.isfinite(ref).all():
        raise RenderError("reference_invalid", "reference window contains non-finite samples")
    # Reference span the block's samples map to (>= 0.1 s) for the level fallback.
    c_lo, c_hi = float(frame_times.min()), float(frame_times.max())
    if c_hi - c_lo < 0.1:
        mid = (c_lo + c_hi) / 2
        c_lo, c_hi = mid - 0.05, mid + 0.05
    core = c.ref.read(int(round(max(0.0, c_lo) * sr)), max(1, int(round(min(dur, c_hi) * sr))))
    fi = (tr.times >= r0) & (tr.times <= r1)
    near = (tr.times >= float(ref_times.min()) - ctx) & (tr.times <= float(ref_times.max()) + ctx)
    if s1 - s0 < sr // 10 or rms(ref) < 1e-7 or not tr.active[fi].any():
        return _level_fallback(mono, core), {"fallback": "level_only"}
    pos = position_trajectory(tr, r0, r1)
    if not pos.has_position:
        return _level_fallback(mono, core), {"fallback": "level_only"}
    ref = resample(ref, sr, c.rate)
    ref_t0 = s0 / sr
    c.check()
    response, rep = rtf_filters(ref, c.rate, ref_t0, pos, c.size, c.hop, c.check)
    c.check()
    spec = stft(mono[:, None], c.size, c.hop)[:, :, 0]
    kt = ref_times[: len(spec)] if len(ref_times) >= len(spec) else np.pad(
        ref_times, (0, len(spec) - len(ref_times)), mode="edge")
    positions = np.clip((kt - ref_t0) * c.rate / c.hop, 0, len(response) - 1)
    lower = np.floor(positions).astype(int)
    upper = np.minimum(lower + 1, len(response) - 1)
    fraction = (positions - lower)[:, None, None]
    response = response[lower] * (1 - fraction) + response[upper] * fraction
    level_window = tr.level_db[near if near.any() else fi]
    base_level = float(np.median(level_window))
    gain = np.clip(10 ** (base_level / 20) / max(rms(mono), 1e-8), 0.01, 30)
    level = np.interp(kt, tr.times, tr.level_db)
    envelope = 10 ** (np.clip(level - base_level, -18, 12) * c.strengths["level"] / 20)
    hz = np.fft.rfftfreq(c.size, 1 / c.rate)
    colors = np.stack([np.interp(kt, tr.times, col) for col in tr.color_db.T], axis=1)
    coloration = np.stack(
        [10 ** (np.interp(hz, tr.band_hz, row) * c.strengths["color"] / 20) for row in colors]
    )
    spectrum = spec[:, :, None] * response * coloration[:, :, None] * (envelope * gain)[:, None, None]
    out = istft(spectrum, c.size, c.hop, len(mono))
    fade = min(round(c.rate * FADE_SECONDS), len(out) // 2)
    if fade:
        ramp = np.linspace(0, 1, fade)[:, None]
        out[:fade] *= ramp
        out[-fade:] *= ramp[::-1]
    if not np.isfinite(out).all():
        raise RenderError("internal", "RTF render produced non-finite samples")
    held = float(np.mean(np.interp(kt, pos.times, pos.held.astype(float)) > 0.5))
    rep["held_frame_fraction"] = held
    return out, rep


def render_array(
    mono: np.ndarray, rate: int, reference: WavReader, track: SpatialTrack, *,
    mode: str = "source_trajectory", window: dict, strengths: dict | None = None,
    progress: Callable[[str, int, int], None] | None = None,
    check_cancel: Callable[[], None] | None = None,
) -> tuple[np.ndarray, dict]:
    """Spatialise a mono clip. Output has exactly ``len(mono)`` frames."""
    if mode not in MODES:
        raise RenderError("invalid_input", f"unknown mode {mode!r}")
    st = resolve_strengths(strengths)
    check = check_cancel or (lambda: None)
    if mono.ndim != 1 or not np.isfinite(mono).all():
        raise RenderError("clip_invalid", "clip contains non-finite samples")
    report = {"mode": mode, "renderer_version": RENDERER_VERSION, "blocks": 0,
              "fallback_blocks": {}, "context_ms": CONTEXT_MS,
              "hold_release_ms": HOLD_RELEASE_MS, "strengths": st}
    n = len(mono)
    if n == 0 or rms(mono) < 1e-8:
        report["fallback_blocks"]["silent_clip"] = 1
        report.update(mean_rtf_confidence=None, reliable_bin_fraction=None,
                      held_frame_fraction=None, fallback_used=True)
        return np.column_stack((mono, mono)).astype(np.float32), report
    dur = track.meta["duration_seconds"]
    src_lo = min(dur, max(0.0, window["src_start_ms"] / 1000))
    src_hi = min(dur, max(src_lo, window["src_end_ms"] / 1000))
    if mode == "concurrent":
        if "dub_start_ms" not in window:
            raise RenderError("invalid_input", "concurrent mode needs window.dub_start_ms")
        dub0 = window["dub_start_ms"] / 1000
    # End of the source trajectory: last position-reliable frame in the sentence.
    in_src = np.flatnonzero((track.times >= src_lo) & (track.times <= src_hi) & track.position_reliable)
    t_end = float(track.times[in_src[-1]]) if len(in_src) else src_hi

    def mapping(j: np.ndarray) -> tuple[np.ndarray, str | None]:
        if mode == "source_trajectory":
            return src_lo + (src_hi - src_lo) * j / n, None
        t = dub0 + j / rate
        sel = (track.times >= t.min()) & (track.times <= t.max())
        frac = float(np.mean(track.active[sel])) if sel.any() else 0.0
        if t.min() >= dur or frac < CONCURRENT_MIN_ACTIVE:
            return np.full_like(j, t_end, dtype=np.float64), "source_end"
        return t, None

    size = 4096 if rate >= 32000 else 2048
    hop = size // 8
    c = _Ctx(track, reference, rate, st, size, hop, check)
    chunk = max(1, round(BLOCK_SECONDS * rate))
    pad = min(round(rate * BLOCK_PAD_SECONDS), chunk // 4)
    result = np.zeros((n, 2))
    weights = np.zeros(n)
    starts = list(range(0, n, chunk))
    confs, bins, helds = [], [], []
    for bi, start in enumerate(starts):
        check()
        end = min(n, start + chunk)
        a, b = max(0, start - pad), min(n, end + pad)
        seg = mono[a:b]
        frame_j = np.minimum(a + np.arange(0, b - a + size, hop), b).astype(np.float64)
        ref_times, why = mapping(frame_j)
        sample_times, _ = mapping(np.array([a, b], dtype=np.float64))
        if rms(seg) < 1e-8:
            audio, info = np.column_stack((seg, seg)), {"fallback": "silent_block"}
        else:
            audio, info = _render_block(c, seg, ref_times, sample_times)
            audio = audio * (rms(seg) / max(rms(audio), 1e-8))
        reason = info.get("fallback") or why
        if reason:
            report["fallback_blocks"][reason] = report["fallback_blocks"].get(reason, 0) + 1
        if "mean_rtf_confidence" in info:
            confs.append(info["mean_rtf_confidence"])
            bins.append(info["reliable_bin_fraction"])
            helds.append(info["held_frame_fraction"])
        weight = np.ones(b - a)
        if a < start:
            weight[: start - a] = np.linspace(0, 1, start - a)
        if b > end:
            weight[end - a :] = np.linspace(1, 0, b - end)
        result[a:b] += audio * weight[:, None]
        weights[a:b] += weight
        report["blocks"] += 1
        if progress:
            progress("render", bi + 1, len(starts))
    result /= np.maximum(weights[:, None], 1e-8)
    result = result * st["rtf"] + mono[:, None] * (1 - st["rtf"])
    report["mean_rtf_confidence"] = float(np.mean(confs)) if confs else None
    report["reliable_bin_fraction"] = float(np.mean(bins)) if bins else None
    report["held_frame_fraction"] = float(np.mean(helds)) if helds else None
    report["fallback_used"] = bool(report["fallback_blocks"])
    if not np.isfinite(result).all():
        raise RenderError("internal", "render produced non-finite samples")
    return result.astype(np.float32), report


def render_file(
    clip_path: str | Path, reference_path: str | Path, track: SpatialTrack, out_path: str | Path,
    **kw,
) -> dict:
    """Render a clip WAV to a stereo float32 WAV at ``out_path`` (atomic within its dir)."""
    audio, rate = wav_read(clip_path)
    if audio.shape[1] > 2:
        raise RenderError("clip_invalid", "clip must be mono or stereo")
    mono = audio.mean(axis=1)
    with WavReader(reference_path) as ref:
        if ref.info.channels != 2:
            raise RenderError("reference_not_stereo", "reference must be stereo")
        if abs(ref.info.frames / ref.info.sample_rate - track.meta["duration_seconds"]) > 1e-3:
            raise RenderError("track_mismatch", "spatial track does not match reference duration")
        out, report = render_array(mono, rate, ref, track, **kw)
    out_path = Path(out_path)
    tmp = out_path.with_name(f".{uuid.uuid4().hex}.tmp.wav")
    try:
        write_float32(tmp, out, rate)
        os.replace(tmp, out_path)
    finally:
        if tmp.exists():
            tmp.unlink()
    report.update(sample_rate=rate, channels=2, frames=int(len(out)),
                  duration_ms=round(len(out) * 1000 / rate), input_channels=int(audio.shape[1]))
    return report
