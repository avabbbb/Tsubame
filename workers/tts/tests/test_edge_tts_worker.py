import io
import json
import os
import sys
import tempfile
import types
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# The pack ships aiohttp and edge-tts; the dev interpreter running these tests may not.
try:
    import aiohttp  # noqa: F401
except ImportError:
    aiohttp = types.ModuleType("aiohttp")

    class ClientError(Exception):
        pass

    aiohttp.ClientError = ClientError
    sys.modules["aiohttp"] = aiohttp
try:
    import edge_tts  # noqa: F401
except ImportError:
    stub = types.ModuleType("edge_tts")
    stub.Communicate = None
    sys.modules["edge_tts"] = stub

import edge_tts_worker as worker  # noqa: E402


class FakeCommunicate:
    calls: list = []

    def __init__(self, text, voice, **kwargs):
        FakeCommunicate.calls.append((text, voice, kwargs))

    async def stream(self):
        yield {"type": "WordBoundary", "offset": 0}
        yield {"type": "audio", "data": b"ID3abc"}
        yield {"type": "audio", "data": b"def"}


class OfflineCommunicate(FakeCommunicate):
    async def stream(self):
        raise worker.aiohttp.ClientError("cannot connect")
        yield  # pragma: no cover - makes this an async generator


class EdgeTtsWorkerTest(unittest.TestCase):
    def setUp(self):
        FakeCommunicate.calls = []
        self.tmp = tempfile.TemporaryDirectory()
        self.out = os.path.join(self.tmp.name, "out.mp3")

    def tearDown(self):
        self.tmp.cleanup()

    def request(self, **overrides) -> bytes:
        req = {"contract_version": 1, "text": "让你在这么冷的天里等我。", "voice": "zh-CN-XiaoyiNeural",
               "rate": "+10%", "pitch": "+0Hz", "volume": "+0%", "output_path": self.out}
        req.update(overrides)
        return json.dumps(req, ensure_ascii=False).encode("utf-8")

    def run_main(self, raw: bytes):
        stdin = io.TextIOWrapper(io.BytesIO(raw), encoding="utf-8")
        stdout = io.StringIO()
        with mock.patch.object(sys, "stdin", stdin), mock.patch.object(sys, "stdout", stdout):
            code = worker.main()
        lines = stdout.getvalue().splitlines()
        self.assertEqual(len(lines), 1)
        return json.loads(lines[0]), code

    def test_success_writes_mp3_and_json(self):
        with mock.patch.object(worker.edge_tts, "Communicate", FakeCommunicate), \
                mock.patch.dict(os.environ, {"HTTPS_PROXY": "proxy-under-test"}):
            result, code = self.run_main(self.request())
        self.assertEqual(code, 0)
        self.assertEqual(result, {"ok": True, "format": "mp3", "bytes": 9})
        with open(self.out, "rb") as f:
            self.assertEqual(f.read(), b"ID3abcdef")
        text, voice, kwargs = FakeCommunicate.calls[0]
        self.assertEqual((text, voice), ("让你在这么冷的天里等我。", "zh-CN-XiaoyiNeural"))
        self.assertEqual(kwargs, {"rate": "+10%", "pitch": "+0Hz", "volume": "+0%", "proxy": "proxy-under-test"})

    def test_text_is_redacted_before_leaving(self):
        with mock.patch.object(worker.edge_tts, "Communicate", FakeCommunicate):
            _, code = worker.run(self.request(text="联系 a.b@mail.example.co.jp"))
        self.assertEqual(code, 0)
        self.assertEqual(FakeCommunicate.calls[0][0], "联系 ⟦EMAIL1⟧")

    def test_empty_text_is_invalid_input(self):
        with mock.patch.object(worker.edge_tts, "Communicate", FakeCommunicate):
            result, code = self.run_main(self.request(text="  "))
        self.assertEqual(code, 1)
        self.assertFalse(result["ok"])
        self.assertEqual(result["error"]["code"], "invalid_input")
        self.assertFalse(result["error"]["retryable"])
        self.assertEqual(FakeCommunicate.calls, [])
        self.assertFalse(os.path.exists(self.out))

    def test_missing_voice_is_invalid_input(self):
        raw = json.loads(self.request())
        del raw["voice"]
        result, code = worker.run(json.dumps(raw).encode("utf-8"))
        self.assertEqual((code, result["error"]["code"]), (1, "invalid_input"))

    def test_client_error_is_retryable_network(self):
        with mock.patch.object(worker.edge_tts, "Communicate", OfflineCommunicate):
            result, code = self.run_main(self.request())
        self.assertEqual(code, 1)
        self.assertEqual(result["error"]["code"], "network")
        self.assertTrue(result["error"]["retryable"])
        self.assertFalse(os.path.exists(self.out))


if __name__ == "__main__":
    unittest.main()
