from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

import numpy as np

from spatial._wav import WavReader, probe, read, write_float32
from spatial.analyze import analyze_file
from spatial.render import position_trajectory, render_array, render_file, render_key
from spatial.tests import signals as sig

RATE = 16000


class Fixture:
    """Writes a reference WAV, analyses it, and renders clips against it."""

    def __init__(self, stereo: np.ndarray, rate: int = RATE):
        self._dir = tempfile.TemporaryDirectory()
        self.dir = Path(self._dir.name)
        self.ref = self.dir / "ref.wav"
        write_float32(self.ref, stereo, rate)
        self.track = analyze_file(self.ref)

    def render(self, mono, rate=RATE, **kw):
        with WavReader(self.ref) as r:
            return render_array(np.asarray(mono, dtype=np.float64), rate, r, self.track, **kw)

    def close(self):
        self._dir.cleanup()


def seg_ild(out, rate, t0, t1):
    return sig.broadband_ild_db(out[int(t0 * rate) : int(t1 * rate)])


class Duration(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.fx = Fixture(sig.panned(sig.voice(RATE, 3.0, seed=5), "left", RATE))

    @classmethod
    def tearDownClass(cls):
        cls.fx.close()

    def test_output_frames_equal_input_frames(self):
        for n in (RATE // 50, 21911, RATE * 3, int(RATE * 12.3)):  # incl. multi-block
            mono = sig.voice(RATE, n / RATE, seed=n)[:n]
            out, rep = self.fx.render(mono, window={"src_start_ms": 500, "src_end_ms": 2500})
            self.assertEqual(out.shape, (n, 2), n)
            self.assertTrue(np.isfinite(out).all())
        self.assertEqual(rep["blocks"], 2)

    def test_other_clip_rate_and_stereo_clip_file(self):
        rate = 24000
        mono = sig.voice(rate, 1.7, seed=8)
        clip = self.fx.dir / "clip.wav"
        write_float32(clip, np.column_stack((mono, mono * 0.5)), rate)
        out = self.fx.dir / "out.wav"
        rep = render_file(clip, self.fx.ref, self.fx.track, out,
                          window={"src_start_ms": 0, "src_end_ms": 3000})
        info = probe(out)
        self.assertEqual((info.sample_rate, info.channels, info.frames), (rate, 2, len(mono)))
        self.assertEqual(rep["frames"], len(mono))
        self.assertEqual(rep["input_channels"], 2)
        audio, _ = read(out)
        self.assertGreater(sig.broadband_ild_db(audio), 4)

    def test_silent_clip_and_dry_strength(self):
        z = np.zeros(RATE)
        out, rep = self.fx.render(z, window={"src_start_ms": 0, "src_end_ms": 1000})
        self.assertTrue((out == 0).all() and rep["fallback_used"])
        mono = sig.voice(RATE, 1.0, seed=9)
        out, _ = self.fx.render(mono, window={"src_start_ms": 0, "src_end_ms": 1000},
                                strengths={"rtf": 0.0})
        np.testing.assert_allclose(out[:, 0], mono, atol=1e-6)
        np.testing.assert_allclose(out[:, 1], mono, atol=1e-6)

    def test_rms_handed_back_to_mix(self):
        mono = sig.voice(RATE, 2.0, seed=10, level=0.2)
        out, _ = self.fx.render(mono, window={"src_start_ms": 0, "src_end_ms": 2000})
        # Blocks are RMS-matched to the dry clip (over both channels together).
        self.assertAlmostEqual(float(np.sqrt(np.mean(out.astype(np.float64) ** 2))),
                               float(np.sqrt(np.mean(mono**2))), delta=0.02)


class Follow(unittest.TestCase):
    def test_source_trajectory_follows_moving_pan_stretched(self):
        fx = Fixture(sig.moving(sig.voice(RATE, 3.0, seed=11), RATE))
        try:
            mono = sig.voice(RATE, 6.0, seed=12)  # clip twice as long as the sentence
            out, rep = fx.render(mono, window={"src_start_ms": 0, "src_end_ms": 3000})
            self.assertEqual(len(out), len(mono))
            first, last = seg_ild(out, RATE, 0.2, 1.2), seg_ild(out, RATE, 4.8, 5.8)
            mid = seg_ild(out, RATE, 2.7, 3.3)
            self.assertGreater(first, 4)
            self.assertLess(last, -4)
            self.assertLess(abs(mid), 3)
            self.assertFalse(rep["fallback_used"])
            # Same pan swapped => mirrored output.
            fx2 = Fixture(sig.moving(sig.voice(RATE, 3.0, seed=11), RATE)[:, ::-1])
            out2, _ = fx2.render(mono, window={"src_start_ms": 0, "src_end_ms": 3000})
            self.assertLess(seg_ild(out2, RATE, 0.2, 1.2), -4)
            fx2.close()
        finally:
            fx.close()

    def test_concurrent_mode_and_inactive_fallback(self):
        x = sig.voice(RATE, 6.0, seed=13)
        ref = np.concatenate([sig.panned(x[: 2 * RATE], "left", RATE),
                              np.zeros((2 * RATE, 2)),
                              sig.panned(x[4 * RATE :], "right", RATE)])
        fx = Fixture(ref)
        try:
            mono = sig.voice(RATE, 1.5, seed=14)
            w = {"src_start_ms": 0, "src_end_ms": 2000}
            out, rep = fx.render(mono, mode="concurrent", window={**w, "dub_start_ms": 4200})
            self.assertLess(sig.broadband_ild_db(out), -4)
            self.assertFalse(rep["fallback_used"])
            out, rep = fx.render(mono, mode="concurrent", window={**w, "dub_start_ms": 2200})
            self.assertGreater(sig.broadband_ild_db(out), 4)  # end of source trajectory
            self.assertEqual(rep["fallback_blocks"], {"source_end": 1})
            out, rep = fx.render(mono, mode="concurrent", window={**w, "dub_start_ms": 60000})
            self.assertGreater(sig.broadband_ild_db(out), 4)  # beyond the file
            self.assertEqual(len(out), len(mono))
            out, _ = fx.render(mono, window={**w, "dub_start_ms": 4200})  # default mode
            self.assertGreater(sig.broadband_ild_db(out), 4)
        finally:
            fx.close()

    def test_silent_reference_window_falls_back_to_level_only(self):
        ref = np.concatenate([sig.panned(sig.voice(RATE, 1.0), "left", RATE), np.zeros((2 * RATE, 2))])
        fx = Fixture(ref)
        try:
            mono = sig.voice(RATE, 1.0, seed=15)
            out, rep = fx.render(mono, window={"src_start_ms": 1600, "src_end_ms": 2600})
            self.assertEqual(rep["fallback_blocks"], {"level_only": 1})
            np.testing.assert_allclose(out[:, 0], out[:, 1], atol=1e-6)
        finally:
            fx.close()


class LowConfidenceHold(unittest.TestCase):
    """Left 0-1.5 s, incoherent noise 1.5-2.5 s, right 2.5-4 s."""

    @classmethod
    def setUpClass(cls):
        x = sig.voice(RATE, 4.0, seed=16)
        ref = np.concatenate([sig.panned(x[: int(1.5 * RATE)], "left", RATE),
                              sig.incoherent(RATE, 1.0),
                              sig.panned(x[int(2.5 * RATE) :], "right", RATE)])
        cls.fx = Fixture(ref)

    @classmethod
    def tearDownClass(cls):
        cls.fx.close()

    def test_track_marks_noise_unreliable(self):
        tr = self.fx.track
        gap = (tr.times > 1.7) & (tr.times < 2.3)
        self.assertLess(np.mean(tr.position_reliable[gap]), 0.2)
        self.assertLess(np.mean(tr.itd_reliable[gap]), 0.2)
        # Dubber-equivalent smoothed ILD goes through the noise (~0 dB) ...
        self.assertLess(abs(np.median(tr.ild_db[gap])), 2)

    def test_trajectory_holds_then_releases(self):
        pos = position_trajectory(self.fx.track, 0.0, 4.0)
        ild = np.median(pos.ild, axis=1)
        gap = (pos.times > 1.7) & (pos.times < 2.3)
        # ... while the held trajectory keeps the last reliable (left) position.
        self.assertGreater(np.min(ild[gap]), 6)
        self.assertGreater(np.min(pos.itd[gap]) * 1000, 0.2)
        after = pos.times > 3.0
        self.assertLess(np.median(ild[after]), -6)
        self.assertLess(np.median(pos.itd[after]) * 1000, -0.2)
        # Release is gradual: no frame-to-frame jump bigger than a few dB.
        self.assertLess(np.max(np.abs(np.diff(ild))), 4.0)

    def test_render_in_noise_window_stays_left(self):
        mono = sig.voice(RATE, 0.8, seed=17)
        out, rep = self.fx.render(mono, window={"src_start_ms": 1600, "src_end_ms": 2400})
        self.assertGreater(sig.broadband_ild_db(out), 4)
        self.assertGreater(rep["held_frame_fraction"], 0.8)


class RenderKey(unittest.TestCase):
    def test_key_contract(self):
        w = {"src_start_ms": 1000, "src_end_ms": 2000, "dub_start_ms": 2300}
        k = render_key("tts", "trk", "source_trajectory", w, None)
        self.assertEqual(k, render_key("tts", "trk", "source_trajectory", {**w, "dub_start_ms": 9000},
                                       {"rtf": 1, "level": 0.7, "color": 0.35}))
        c = render_key("tts", "trk", "concurrent", w, None)
        self.assertNotEqual(c, k)
        self.assertNotEqual(c, render_key("tts", "trk", "concurrent", {**w, "dub_start_ms": 9000}, None))
        # Pinned cross-language vectors: the Rust host must produce the same hex.
        w0 = {"src_start_ms": 1000, "src_end_ms": 2500, "dub_start_ms": 2800}
        self.assertEqual(render_key("tts:abc", "0" * 64, "source_trajectory", w0, None, "spatial-render/1"),
                         "a77b7081a3038637b6ff0199f11aad0eeaabddfd61f1e89c12c4b9a321afdb4e")
        self.assertEqual(render_key("tts:abc", "0" * 64, "concurrent", w0, None, "spatial-render/1"),
                         "05fb51ff4c93a080b5d5ab75046f4a3a3493fa3331a85a4cb65974964c1edd8e")
        for other in (render_key("tts2", "trk", "source_trajectory", w, None),
                      render_key("tts", "trk2", "source_trajectory", w, None),
                      render_key("tts", "trk", "source_trajectory", {**w, "src_end_ms": 2001}, None),
                      render_key("tts", "trk", "source_trajectory", w, {"color": 0.5}),
                      render_key("tts", "trk", "source_trajectory", w, None, "spatial-render/999")):
            self.assertNotEqual(k, other)


if __name__ == "__main__":
    unittest.main()
