import os
import struct
import sys
import unittest

HERE = os.path.dirname(os.path.abspath(globals().get("_" + "_file_" + "_", "")))
sys.path.insert(0, os.path.dirname(HERE))

from edge_tts_shim import fix_wav_sizes  # noqa: E402


def streamed_wav(payload: bytes) -> bytes:
    """WAV as ffmpeg writes it to a pipe: RIFF and data sizes left at 0xFFFFFFFF."""
    fmt = struct.pack("<HHIIHH", 1, 1, 24000, 48000, 2, 16)
    return (b"RIFF" + struct.pack("<I", 0xFFFFFFFF) + b"WAVE" + b"fmt " + struct.pack("<I", 16) + fmt
            + b"LIST" + struct.pack("<I", 4) + b"INFO" + b"data" + struct.pack("<I", 0xFFFFFFFF) + payload)


class FixWavSizesTest(unittest.TestCase):
    def test_rewrites_riff_and_data_sizes(self):
        payload = bytes(4800)
        out = fix_wav_sizes(streamed_wav(payload))
        self.assertEqual(struct.unpack_from("<I", out, 4)[0], len(out) - 8)
        pos = out.index(b"data")
        self.assertEqual(struct.unpack_from("<I", out, pos + 4)[0], len(payload))
        self.assertEqual(out[pos + 8:], payload)

    def test_duration_readable_by_wave(self):
        import io, wave
        with wave.open(io.BytesIO(fix_wav_sizes(streamed_wav(bytes(48000))))) as w:
            self.assertAlmostEqual(w.getnframes() / w.getframerate(), 1.0)

    def test_non_wav_passthrough(self):
        self.assertEqual(fix_wav_sizes(b"not a wav"), b"not a wav")
