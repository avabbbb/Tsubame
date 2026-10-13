"""Golden agreement with ASMR-Dubber's own implementation (commit 2faea94f).

Imports Dubber from a local checkout *in this test only*; skipped when it is not
available. Set ``TSUBAME_DUBBER_SRC`` to the checkout's ``src`` directory
(default ``/workspace/work/asmr-dubber/src``). Nothing from Dubber is vendored.
"""

from __future__ import annotations

import os
import sys
import tempfile
import unittest
from pathlib import Path

import numpy as np

from spatial._wav import WavReader, write_float32
from spatial.analyze import analyze_array, analyze_file, rms
from spatial.render import render_array
from spatial.tests import signals as sig

DUBBER_SRC = os.environ.get("TSUBAME_DUBBER_SRC", "/workspace/work/asmr-dubber/src")
DEMOS = Path(DUBBER_SRC).parent / "assets" / "demos"

try:
    sys.path.insert(0, DUBBER_SRC)
    from asmr_dubber.spatial_cues import analyze as dubber_analyze  # type: ignore
    from asmr_dubber.spatial_rtf import render_rtf as dubber_render_rtf  # type: ignore
except Exception as exc:  # noqa: BLE001
    dubber_analyze = dubber_render_rtf = None
    SKIP = f"ASMR-Dubber not importable from {DUBBER_SRC}: {exc}"
else:
    SKIP = ""
finally:
    if sys.path and sys.path[0] == DUBBER_SRC:
        sys.path.pop(0)

FIELDS = ("ild_db", "itd_seconds", "level_db", "color_db", "confidence")
# Per-field absolute tolerance (dB, seconds, dB, dB, unitless).
TOL = {"ild_db": 1e-9, "itd_seconds": 1e-12, "level_db": 1e-9, "color_db": 1e-9, "confidence": 1e-9}
AGREEMENT: dict = {}  # filled for the report printed by tearDownModule


def _dubber_lab_reference() -> np.ndarray:
    """Reference of Dubber's ``test_rtf_direction_and_swap_match_lab``."""
    s = np.random.default_rng(11).normal(0, 0.03, 24000)
    return np.column_stack((s, np.pad(s, (8, 0))[: len(s)] * 0.5))


def _compare(test, name, ours, theirs):
    diffs = {}
    np.testing.assert_allclose(ours.times, theirs.times, atol=1e-12)
    np.testing.assert_allclose(ours.band_hz, theirs.band_hz, rtol=1e-12)
    for f in FIELDS:
        d = float(np.max(np.abs(getattr(ours, f) - getattr(theirs, f))))
        diffs[f] = d
        test.assertLessEqual(d, TOL[f], f"{name}: {f} differs by {d}")
    AGREEMENT[name] = diffs


@unittest.skipIf(dubber_analyze is None, SKIP)
class GoldenAnalyze(unittest.TestCase):
    def test_synthetic_inputs(self):
        cases = {
            "lab_left_24k": (sig.panned(sig.voice(24000, 2.0, seed=21), "left", 24000), 24000),
            "moving_48k": (sig.moving(sig.voice(48000, 3.0, seed=22), 48000), 48000),
            "right_with_silence_16k": (np.concatenate([
                np.zeros((8000, 2)), sig.panned(sig.voice(16000, 1.5, seed=23), "right", 16000),
                np.zeros((12000, 2)), sig.incoherent(16000, 0.5)]), 16000),
            "dubber_lab_24k": (_dubber_lab_reference(), 24000),
        }
        for name, (audio, rate) in cases.items():
            _compare(self, name, analyze_array(audio, rate), dubber_analyze(audio, rate))

    def test_real_demo_audio_streamed_from_file(self):
        demo = DEMOS / "demo-1.wav"
        if not demo.is_file():
            self.skipTest("Dubber demo asset not present")
        with WavReader(demo) as r:
            n = min(r.info.frames, r.info.sample_rate * 8)
            audio = r.read(0, n)
            rate = r.info.sample_rate
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "demo8s.wav"
            write_float32(p, audio, rate)
            exact = WavReader(p).read(0, n)
            _compare(self, "demo-1_first8s_48k_file", analyze_file(p), dubber_analyze(exact, rate))


@unittest.skipIf(dubber_render_rtf is None, SKIP)
class GoldenRender(unittest.TestCase):
    """Whole-reference window, clip length == window length, all frames reliable:
    our cached-track render must reproduce Dubber's ``render_rtf`` (after the
    block RMS hand-back that Dubber's ``spatialize_clip`` applies)."""

    def _case(self, name, ref, mono, rate):
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "ref.wav"
            write_float32(p, ref, rate)
            with WavReader(p) as r:
                exact = r.read(0, len(ref))
                ours, rep = render_array(mono, rate, r, analyze_file(p),
                                         window={"src_start_ms": 0,
                                                 "src_end_ms": round(len(ref) * 1000 / rate)})
        theirs, _ = dubber_render_rtf(mono, exact, rate)
        theirs = theirs * (rms(mono) / rms(theirs))
        rel = rms(ours - theirs) / rms(theirs)
        ild_o, ild_t = sig.broadband_ild_db(ours), sig.broadband_ild_db(theirs)
        corr = [float(np.corrcoef(ours[:, c], theirs[:, c])[0, 1]) for c in (0, 1)]
        AGREEMENT[name] = {"relative_rms_error": float(rel), "ild_ours_db": ild_o,
                           "ild_dubber_db": ild_t, "channel_correlation": corr,
                           "mean_rtf_confidence": rep["mean_rtf_confidence"]}
        self.assertFalse(rep["fallback_used"])
        self.assertLess(rel, 0.01, name)
        self.assertLess(abs(ild_o - ild_t), 0.05, name)
        self.assertGreater(min(corr), 0.9999, name)

    def test_dubber_lab_case(self):
        rng = np.random.default_rng(11)
        rng.normal(0, 0.03, 24000)  # same draw order as Dubber's lab test
        self._case("render_dubber_lab_24k", _dubber_lab_reference(), rng.normal(0, 0.03, 24000), 24000)

    def test_moving_source(self):
        ref = sig.moving(sig.voice(24000, 2.5, seed=31), 24000)
        self._case("render_moving_24k", ref, sig.voice(24000, 2.5, seed=32), 24000)


def tearDownModule():
    if AGREEMENT and os.environ.get("TSUBAME_GOLDEN_REPORT"):
        import json

        print("\nGOLDEN " + json.dumps(AGREEMENT, indent=1), file=sys.stderr)


if __name__ == "__main__":
    unittest.main()
