"""Synthetic stereo test signals (speech-like modulated noise)."""

from __future__ import annotations

import numpy as np


def voice(rate: int, seconds: float, seed: int = 0, level: float = 0.05) -> np.ndarray:
    rng = np.random.default_rng(seed)
    n = int(round(rate * seconds))
    x = rng.normal(0, 1, n)
    # Gentle low-pass tilt so it is not perfectly white, then syllable-rate AM.
    x = np.convolve(x, np.ones(3) / 3, mode="same")
    t = np.arange(n) / rate
    env = 0.6 + 0.4 * np.abs(np.sin(2 * np.pi * 2.5 * t))
    return level * x * env / np.std(x)


def delayed(x: np.ndarray, samples: float) -> np.ndarray:
    """Fractional delay by linear interpolation (positive = later)."""
    idx = np.arange(len(x)) - samples
    return np.interp(idx, np.arange(len(x)), x, left=0.0, right=0.0)


def panned(x: np.ndarray, side: str, rate: int, ild_db: float = 9.0, itd_ms: float = 0.3) -> np.ndarray:
    """Source to one side: far ear quieter by ``ild_db`` and later by ``itd_ms``."""
    far = delayed(x, itd_ms / 1000 * rate) * 10 ** (-ild_db / 20)
    return np.column_stack((x, far)) if side == "left" else np.column_stack((far, x))


def hard(x: np.ndarray, side: str) -> np.ndarray:
    z = np.zeros_like(x)
    return np.column_stack((x, z)) if side == "left" else np.column_stack((z, x))


def moving(x: np.ndarray, rate: int, ild_db: float = 9.0, itd_ms: float = 0.3) -> np.ndarray:
    """Source sweeping smoothly from left to right over the signal."""
    n = len(x)
    p = np.linspace(1, -1, n)  # +1 left ... -1 right
    d = p * itd_ms / 1000 * rate  # positive => right ear delayed
    pos = np.arange(n, dtype=np.float64)
    left = np.interp(pos - np.maximum(-d, 0), pos, x, left=0, right=0)
    right = np.interp(pos - np.maximum(d, 0), pos, x, left=0, right=0)
    g = 10 ** (-np.abs(p) * ild_db / 20)
    left *= np.where(p < 0, g, 1)
    right *= np.where(p > 0, g, 1)
    return np.column_stack((left, right))


def incoherent(rate: int, seconds: float, seed: int = 99, level: float = 0.05) -> np.ndarray:
    return np.column_stack((voice(rate, seconds, seed, level), voice(rate, seconds, seed + 1, level)))


def broadband_ild_db(stereo: np.ndarray) -> float:
    l = np.sqrt(np.mean(stereo[:, 0] ** 2))
    r = np.sqrt(np.mean(stereo[:, 1] ** 2))
    return float(20 * np.log10(max(l, 1e-12) / max(r, 1e-12)))
