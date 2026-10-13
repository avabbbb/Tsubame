from __future__ import annotations

import os
import struct
import tempfile
import unittest
import wave
from pathlib import Path

import numpy as np

from spatial import ANALYZER_VERSION
from spatial._wav import WavReader, probe, write_float32
from spatial.analyze import (
    AnalysisError, DEFAULT_PARAMS, SpatialTrack, analyze_array, analyze_file, spatial_track_key,
)
from spatial.tests import signals as sig

RATE = 24000


class AnalyzeDirection(unittest.TestCase):
    def test_left_and_right_signs(self):
        x = sig.voice(RATE, 2.0, seed=1)
        for side, sign in (("left", 1), ("right", -1)):
            tr = analyze_array(sig.panned(x, side, RATE, ild_db=9, itd_ms=0.3), RATE)
            ild = np.median(tr.ild_db[tr.active])
            itd_ms = np.median(tr.itd_seconds[tr.active]) * 1000
            self.assertGreater(sign * ild, 6.0, side)
            self.assertAlmostEqual(sign * itd_ms, 0.3, delta=0.05)
            self.assertGreater(np.mean(tr.itd_reliable[tr.active]), 0.9)
            self.assertGreater(np.mean(tr.position_reliable[tr.active]), 0.9)

    def test_hard_pan_is_position_reliable_by_ild(self):
        x = sig.voice(RATE, 1.5, seed=2)
        tr = analyze_array(sig.hard(x, "right"), RATE)
        self.assertLess(np.median(tr.ild_db[tr.active]), -20)
        self.assertFalse(tr.itd_reliable.any())  # one silent ear: no coherence
        self.assertGreater(np.mean(tr.position_reliable[tr.active]), 0.9)

    def test_moving_trajectory(self):
        x = sig.voice(RATE, 4.0, seed=3)
        tr = analyze_array(sig.moving(x, RATE), RATE)
        ild = np.median(tr.ild_db, axis=1)
        n = len(ild)
        self.assertGreater(np.median(ild[: n // 6]), 4)
        self.assertLess(np.median(ild[-n // 6 :]), -4)
        self.assertLess(np.corrcoef(tr.times, ild)[0, 1], -0.95)
        self.assertLess(np.corrcoef(tr.times, tr.itd_seconds)[0, 1], -0.95)
        self.assertGreater(tr.itd_seconds[n // 10], 0)
        self.assertLess(tr.itd_seconds[-n // 10], 0)

    def test_frame_grid(self):
        tr = analyze_array(sig.panned(sig.voice(48000, 1.0), "left", 48000), 48000)
        self.assertEqual(tr.hop, 960)  # 20 ms
        self.assertEqual(tr.frame_size, 4096)
        self.assertEqual(tr.ild_db.shape, (len(tr.times), 28))
        self.assertEqual(len(tr.times), int(np.ceil(48000 / 960)))


class AnalyzeErrors(unittest.TestCase):
    def test_rejections(self):
        cases = [
            (np.zeros((RATE, 2)), "reference_silent"),
            (sig.voice(RATE, 1.0)[:, None], "reference_not_stereo"),
            (np.ones((RATE // 20, 2)) * 0.1, "reference_too_short"),
        ]
        for audio, code in cases:
            with self.assertRaises(AnalysisError) as cm:
                analyze_array(audio, RATE)
            self.assertEqual(cm.exception.code, code)

    def test_bad_params(self):
        for params in ({"nope": 1}, {"smoothing": {"itd": 0}}, {"hop_ms": "20"}, {"bands": 2},
                       {"hop_ms": 12.5}, {"itd_max_ms": float("nan")}):
            with self.assertRaises(AnalysisError):
                analyze_array(sig.hard(sig.voice(RATE, 1.0), "left"), RATE, params)


class TrackKeyAndPersistence(unittest.TestCase):
    def test_key_contract(self):
        sha = "a" * 64
        k = spatial_track_key(sha, "mix", None)
        self.assertEqual(k, spatial_track_key(sha.upper(), "mix", dict(DEFAULT_PARAMS)))
        self.assertEqual(k, spatial_track_key(sha, "mix", {"hop_ms": 20}))
        self.assertNotEqual(k, spatial_track_key(sha, "vocals", None))
        self.assertNotEqual(k, spatial_track_key("b" * 64, "mix", None))
        self.assertNotEqual(k, spatial_track_key(sha, "mix", {"smoothing": {"ild": 11}}))
        self.assertNotEqual(k, spatial_track_key(sha, "mix", None, "spatial-analyze/999"))
        self.assertRegex(k, r"^[0-9a-f]{64}$")
        self.assertEqual(spatial_track_key(sha, "mix", {"hop_ms": 20.0, "itd_max_ms": 0.9}), k)

    def test_key_vector(self):
        # Pinned cross-language vector: the Rust host must produce the same hex.
        self.assertEqual(
            spatial_track_key("0" * 64, "mix", None, "spatial-analyze/1"),
            "1384a1916325c0effe1fca42d957658fd6f3b783aa22b24801fb6df2d0167923",
        )

    def test_save_load_roundtrip_and_file_analysis(self):
        x = sig.panned(sig.voice(RATE, 1.5, seed=4), "left", RATE)
        with tempfile.TemporaryDirectory() as d:
            p = Path(d) / "ref.wav"
            write_float32(p, x, RATE)
            tr = analyze_file(p)
            ref = analyze_array(x.astype(np.float32).astype(np.float64), RATE)
            np.testing.assert_allclose(tr.ild_db, ref.ild_db, atol=1e-9)
            tr.save(Path(d) / "t.npz")
            back = SpatialTrack.load(Path(d) / "t.npz")
            self.assertEqual(back.meta["analyzer_version"], ANALYZER_VERSION)
            np.testing.assert_allclose(back.ild_db, tr.ild_db, atol=1e-4)
            np.testing.assert_allclose(back.itd_seconds, tr.itd_seconds, atol=1e-12)
            np.testing.assert_array_equal(back.position_reliable, tr.position_reliable)
            self.assertEqual(back.summary()["frames"], len(tr.times))


class WavIO(unittest.TestCase):
    def _pcm(self, path, data, width):
        scale = {2: 32767, 3: 8388607}[width]
        ints = np.round(np.clip(data, -1, 1) * scale).astype(np.int64)
        with wave.open(str(path), "wb") as w:
            w.setnchannels(data.shape[1])
            w.setsampwidth(width)
            w.setframerate(RATE)
            if width == 2:
                w.writeframes(ints.astype("<i2").tobytes())
            else:
                b = ints.reshape(-1)
                raw = bytearray()
                for v in b:
                    raw += struct.pack("<i", int(v))[:3]
                w.writeframes(bytes(raw))

    def test_pcm16_pcm24_float(self):
        data = np.column_stack((np.linspace(-0.5, 0.5, 300), np.linspace(0.25, -0.25, 300)))
        with tempfile.TemporaryDirectory() as d:
            for width, tol in ((2, 1 / 32767), (3, 1 / 8388607)):
                p = Path(d) / f"pcm{width}.wav"
                self._pcm(p, data, width)
                with WavReader(p) as r:
                    self.assertEqual((r.info.channels, r.info.frames), (2, 300))
                    np.testing.assert_allclose(r.read(0, 300), data, atol=tol * 1.01)
                    edge = r.read(-5, 5)
                    self.assertTrue((edge[:5] == 0).all())
            p = Path(d) / "f.wav"
            write_float32(p, data, RATE)
            info = probe(p)
            self.assertEqual((info.fmt, info.bits, info.frames), (3, 32, 300))
            self.assertEqual(os.path.getsize(p), info.data_offset + 300 * 8)
            riff_size = struct.unpack("<I", p.read_bytes()[4:8])[0]
            self.assertEqual(riff_size, os.path.getsize(p) - 8)


if __name__ == "__main__":
    unittest.main()
