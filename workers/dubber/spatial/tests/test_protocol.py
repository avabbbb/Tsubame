"""Protocol v1 conformance: spawns worker.py and speaks JSON Lines over stdio."""

from __future__ import annotations

import json
import os
import queue
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from pathlib import Path

from spatial import ANALYZER_VERSION, RENDERER_VERSION
from spatial._wav import probe, write_float32
from spatial.analyze import spatial_track_key
from spatial.render import render_key
from spatial.tests import signals as sig

WORKER = Path(__file__).resolve().parents[1] / "worker.py"
RATE = 16000


class WorkerProcess:
    def __init__(self):
        env = dict(os.environ)
        env["PYTHONPATH"] = os.pathsep.join(p for p in sys.path if p)
        self.p = subprocess.Popen([sys.executable, str(WORKER)], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
                                  env=env, bufsize=1)
        self.q: queue.Queue = queue.Queue()
        self.raw: list[str] = []
        self._pump_thread = threading.Thread(target=self._pump, daemon=True)
        self._pump_thread.start()

    def _pump(self):
        for line in self.p.stdout:
            self.raw.append(line)
            self.q.put(json.loads(line))
        self.q.put(None)

    def send(self, msg: dict):
        self.p.stdin.write(json.dumps(msg) + "\n")
        self.p.stdin.flush()

    def next(self, timeout=30.0):
        msg = self.q.get(timeout=timeout)
        if msg is None:
            raise AssertionError("worker closed stdout; stderr:\n" + self.p.stderr.read())
        return msg

    def until_terminal(self, jid, timeout=60.0):
        seen = []
        deadline = time.monotonic() + timeout
        while True:
            m = self.next(max(0.1, deadline - time.monotonic()))
            self_id = m.get("id")
            if self_id == jid:
                seen.append(m)
                if m["type"] in ("result", "error"):
                    return seen

    def close(self):
        if self.p.poll() is not None and self.p.stdin.closed:
            return self.p.returncode
        try:
            self.send({"type": "shutdown"})
            self.p.stdin.close()
        except (BrokenPipeError, ValueError):
            pass
        code = self.p.wait(timeout=10)
        self._pump_thread.join(timeout=5)  # drain every line before inspecting raw
        self.p.stdout.close()
        self.p.stderr.close()
        return code


class Protocol(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls._tmp = tempfile.TemporaryDirectory()
        cls.dir = Path(cls._tmp.name)
        cls.ref = cls.dir / "ref.wav"
        write_float32(cls.ref, sig.moving(sig.voice(RATE, 3.0, seed=41), RATE), RATE)
        cls.clip = cls.dir / "clip.wav"
        write_float32(cls.clip, sig.voice(RATE, 1.3, seed=42), RATE)
        cls.out = cls.dir / "out"
        cls.out.mkdir()

    @classmethod
    def tearDownClass(cls):
        cls._tmp.cleanup()

    def setUp(self):
        self.w = WorkerProcess()
        self.hello = self.w.next()

    def tearDown(self):
        self.assertEqual(self.w.close(), 0)
        for line in self.w.raw:  # stdout is protocol only
            self.assertIn(json.loads(line)["type"], {"hello", "progress", "log", "result", "error"})

    def analyze(self, jid="a1", **extra):
        self.w.send({"type": "request", "id": jid, "op": "audio.spatialize",
                     "input": {"op": "analyze", "reference_path": str(self.ref),
                               "reference_kind": "mix", "reference_sha256": "ab" * 32,
                               "out_dir": str(self.out), **extra}})
        return self.w.until_terminal(jid)

    def test_hello(self):
        h = self.hello
        self.assertEqual((h["type"], h["protocol"], h["worker"]), ("hello", 1, "tsubame-spatial"))
        self.assertEqual(h["devices"], ["cpu"])
        cap = h["capabilities"][0]
        self.assertEqual(cap["id"], "audio.spatialize")
        self.assertEqual(cap["features"]["ops"], ["analyze", "render"])
        self.assertEqual(cap["features"]["analyzer_version"], ANALYZER_VERSION)
        self.assertEqual(cap["features"]["renderer_version"], RENDERER_VERSION)

    def test_analyze_then_render(self):
        msgs = self.analyze()
        kinds = [m["type"] for m in msgs]
        self.assertIn("progress", kinds)
        self.assertEqual(kinds[-1], "result", msgs[-1])
        prog = [m for m in msgs if m["type"] == "progress"]
        self.assertTrue(all({"stage", "done", "total"} <= m.keys() for m in prog))
        out = msgs[-1]["output"]
        self.assertEqual(out["spatial_track_key"], spatial_track_key("ab" * 32, "mix", None))
        self.assertTrue(Path(out["track_path"]).is_file())
        self.assertTrue(Path(out["track_path"]).parent == self.out)
        summary = json.loads(Path(out["summary_path"]).read_text())
        self.assertEqual(summary["spatial_track_key"], out["spatial_track_key"])
        self.assertEqual(out["frames"], 150)
        self.assertEqual(out["duration_ms"], 3000)

        window = {"src_start_ms": 0, "src_end_ms": 3000, "dub_start_ms": 3500}
        self.w.send({"type": "request", "id": "r1", "op": "audio.spatialize",
                     "input": {"op": "render", "clip_path": str(self.clip),
                               "reference_path": str(self.ref),
                               "spatial_track_path": out["track_path"],
                               "spatial_track_key": out["spatial_track_key"],
                               "tts_asset_key": "tts-123", "mode": "source_trajectory",
                               "window": window, "out_dir": str(self.out),
                               "output_name": "s1.wav"}})
        msgs = self.w.until_terminal("r1")
        self.assertEqual(msgs[-1]["type"], "result", msgs[-1])
        r = msgs[-1]["output"]
        self.assertEqual(r["path"], str(self.out / "s1.wav"))
        self.assertEqual(r["render_key"], render_key("tts-123", out["spatial_track_key"],
                                                     "source_trajectory", window, None))
        info = probe(r["path"])
        self.assertEqual((info.channels, info.frames, r["frames"]), (2, probe(self.clip).frames, info.frames))
        self.assertEqual(r["report"]["mode"], "source_trajectory")
        self.assertFalse(r["report"]["fallback_used"])
        self.assertEqual([p.name for p in self.out.iterdir() if p.name.startswith(".")], [])

    def test_errors(self):
        cases = [
            ({"op": "audio.mix", "input": {"op": "analyze"}}, "unsupported_op"),
            ({"op": "audio.spatialize", "input": {"op": "explode"}}, "unsupported_op"),
            ({"op": "audio.spatialize", "input": {"op": "analyze",
                                                  "reference_path": str(self.dir / "nope.wav"),
                                                  "reference_kind": "mix", "out_dir": str(self.out)}},
             "input_missing"),
            ({"op": "audio.spatialize", "input": {"op": "analyze", "reference_path": str(self.ref),
                                                  "reference_kind": "karaoke", "out_dir": str(self.out)}},
             "invalid_input"),
            ({"op": "audio.spatialize", "input": {"op": "analyze", "reference_path": str(self.clip),
                                                  "reference_kind": "mix", "out_dir": str(self.out)}},
             "reference_not_stereo"),
            ({"op": "audio.spatialize", "input": {"op": "render", "clip_path": str(self.clip),
                                                  "reference_path": str(self.ref),
                                                  "spatial_track_path": str(self.clip),
                                                  "window": {"src_start_ms": 5, "src_end_ms": 1},
                                                  "out_dir": str(self.out)}}, "invalid_input"),
            ({"op": "audio.spatialize", "input": "nope"}, "invalid_input"),
        ]
        for i, (req, code) in enumerate(cases):
            jid = f"e{i}"
            self.w.send({"type": "request", "id": jid, **req})
            m = self.w.until_terminal(jid)[-1]
            self.assertEqual((m["type"], m["code"]), ("error", code), m)
            self.assertIsInstance(m["retryable"], bool)
            self.assertTrue(m["message"])
        self.w.p.stdin.write("this is not json\n")  # ignored, worker keeps serving
        self.w.p.stdin.flush()
        self.assertEqual(self.analyze("after-garbage")[-1]["type"], "result")

    def test_cancel_running_and_queued(self):
        big = self.dir / "big.wav"
        if not big.exists():
            write_float32(big, sig.panned(sig.voice(48000, 150.0, seed=43), "left", 48000), 48000)
        self.w.send({"type": "request", "id": "long", "op": "audio.spatialize",
                     "input": {"op": "analyze", "reference_path": str(big), "reference_kind": "mix",
                               "reference_sha256": "cd" * 32, "out_dir": str(self.out)}})
        self.w.send({"type": "request", "id": "queued", "op": "audio.spatialize",
                     "input": {"op": "analyze", "reference_path": str(big), "reference_kind": "mix",
                               "reference_sha256": "cd" * 32, "out_dir": str(self.out)}})
        first = self.w.next()
        self.assertEqual((first["id"], first["type"]), ("long", "progress"))
        sent = time.monotonic()
        self.w.send({"type": "cancel", "id": "long"})
        msgs = self.w.until_terminal("long", timeout=2.0)
        self.assertLess(time.monotonic() - sent, 2.0)
        self.assertEqual((msgs[-1]["type"], msgs[-1]["code"]), ("error", "cancelled"))
        self.w.send({"type": "cancel", "id": "queued"})
        # The queued job either got cancelled before starting or while running.
        m = self.w.until_terminal("queued", timeout=2.0)[-1]
        self.assertEqual((m["type"], m["code"]), ("error", "cancelled"))
        # No late result for cancelled ids, and the worker still serves requests.
        self.assertEqual(self.analyze("after-cancel")[-1]["type"], "result")
        late = [json.loads(x) for x in self.w.raw]
        terminal = [m for m in late if m.get("id") in ("long", "queued") and m["type"] in ("result", "error")]
        self.assertEqual(len(terminal), 2)

    def test_shutdown_while_running_exits_cleanly(self):
        big = self.dir / "big2.wav"
        write_float32(big, sig.panned(sig.voice(48000, 60.0, seed=44), "left", 48000), 48000)
        self.w.send({"type": "request", "id": "s", "op": "audio.spatialize",
                     "input": {"op": "analyze", "reference_path": str(big), "reference_kind": "mix",
                               "out_dir": str(self.out)}})
        self.w.next()  # log (sha warning) or progress
        self.assertEqual(self.w.close(), 0)
        msgs = [json.loads(x) for x in self.w.raw if json.loads(x).get("id") == "s"]
        self.assertEqual((msgs[-1]["type"], msgs[-1]["code"]), ("error", "cancelled"))
        self.assertTrue(msgs[-1]["retryable"])


class BatchMode(unittest.TestCase):
    def test_stdin_eof_drains_queue(self):
        with tempfile.TemporaryDirectory() as d:
            ref = Path(d) / "ref.wav"
            write_float32(ref, sig.panned(sig.voice(RATE, 1.0, seed=45), "right", RATE), RATE)
            req = {"type": "request", "id": "b1", "op": "audio.spatialize",
                   "input": {"op": "analyze", "reference_path": str(ref), "reference_kind": "vocals",
                             "reference_sha256": "ef" * 32, "out_dir": d}}
            env = dict(os.environ, PYTHONPATH=os.pathsep.join(p for p in sys.path if p))
            done = subprocess.run([sys.executable, str(WORKER)], input=json.dumps(req) + "\n",
                                  capture_output=True, text=True, env=env, timeout=60)
            self.assertEqual(done.returncode, 0, done.stderr)
            msgs = [json.loads(x) for x in done.stdout.splitlines()]
            self.assertEqual(msgs[0]["type"], "hello")
            self.assertEqual(msgs[-1]["type"], "result")
            self.assertLess(msgs[-1]["output"]["summary"]["median_ild_db_range"][1], -6)


if __name__ == "__main__":
    unittest.main()
