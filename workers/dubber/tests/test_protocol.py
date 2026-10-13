"""Unit tests for the Tsubame worker SDK (protocol v1).

Run: python3 -m unittest discover -s workers/dubber/tests -v
"""

from __future__ import annotations

import io
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
sys.path.insert(0, ROOT)

from tsubame_worker import PROTOCOL_VERSION, Context, Worker, WorkerError, resolve_out_path  # noqa: E402

ECHO = os.path.join(ROOT, "echo_worker.py")


def run_in_process(worker: Worker, host_lines):
    stdin = io.StringIO("".join(json.dumps(line) + "\n" for line in host_lines))
    stdout = io.StringIO()
    worker._stdin = stdin
    worker._stdout = stdout
    assert worker.run() == 0
    return [json.loads(line) for line in stdout.getvalue().splitlines()]


class InProcessTests(unittest.TestCase):
    def make(self) -> Worker:
        worker = Worker("t", "1.0.0", {"test.op": {"x": 1}})

        @worker.handler("test.op")
        def op(ctx: Context):
            if ctx.input.get("fail"):
                raise WorkerError("nope", "failed on purpose", retryable=True)
            if ctx.input.get("explode"):
                raise RuntimeError("kaboom")
            ctx.progress(1, 2, "a")
            ctx.log("warn", "careful")
            ctx.progress(2, 2, "a")
            return {"ok": ctx.input["value"]}

        return worker

    def test_hello_shape(self):
        out = run_in_process(self.make(), [{"type": "shutdown"}])
        self.assertEqual(
            out,
            [
                {
                    "type": "hello",
                    "protocol": PROTOCOL_VERSION,
                    "worker": "t",
                    "version": "1.0.0",
                    "capabilities": [{"id": "test.op", "features": {"x": 1}}],
                    "devices": ["cpu"],
                }
            ],
        )

    def test_request_progress_log_result(self):
        out = run_in_process(
            self.make(),
            [{"type": "request", "id": "j1", "op": "test.op", "input": {"value": 7}}, {"type": "shutdown"}],
        )
        self.assertEqual([m["type"] for m in out], ["hello", "progress", "log", "progress", "result"])
        self.assertEqual(out[1], {"type": "progress", "id": "j1", "stage": "a", "done": 1, "total": 2})
        self.assertEqual(out[2]["level"], "warn")
        self.assertEqual(out[-1], {"type": "result", "id": "j1", "output": {"ok": 7}})

    def test_structured_and_unexpected_errors(self):
        out = run_in_process(
            self.make(),
            [
                {"type": "request", "id": "a", "op": "test.op", "input": {"fail": True}},
                {"type": "request", "id": "b", "op": "test.op", "input": {"explode": True}},
                {"type": "request", "id": "c", "op": "other.op", "input": {}},
            ],
        )
        errors = {m["id"]: m for m in out if m["type"] == "error"}
        self.assertEqual((errors["a"]["code"], errors["a"]["retryable"]), ("nope", True))
        self.assertEqual((errors["b"]["code"], errors["b"]["retryable"]), ("internal", False))
        self.assertIn("kaboom", errors["b"]["message"])
        self.assertEqual(errors["c"]["code"], "unsupported_op")

    def test_resolve_out_path_rejects_escape(self):
        with tempfile.TemporaryDirectory() as tmp:
            ctx = Context(None, "x", "op", {"out_dir": tmp})  # type: ignore[arg-type]
            self.assertTrue(resolve_out_path(ctx, "a/b.wav").startswith(os.path.realpath(tmp)))
            with self.assertRaises(WorkerError):
                resolve_out_path(ctx, "../evil.wav")
            with self.assertRaises(WorkerError):
                resolve_out_path(ctx, "/etc/passwd")


    def test_non_object_input_is_invalid_input(self):
        out = run_in_process(
            self.make(),
            [{"type": "request", "id": "x", "op": "test.op", "input": "nope"}],
        )
        error = [m for m in out if m["type"] == "error"][0]
        self.assertEqual((error["id"], error["code"], error["retryable"]), ("x", "invalid_input", False))

    def test_cancel_of_queued_request_skips_the_handler(self):
        worker = Worker("t", "1.0.0", ["test.slow"])
        started = []

        @worker.handler("test.slow")
        def slow(ctx: Context):
            started.append(ctx.id)
            time.sleep(0.3)
            return {"id": ctx.id}

        out = run_in_process(
            worker,
            [
                {"type": "request", "id": "first", "op": "test.slow", "input": {}},
                {"type": "request", "id": "queued", "op": "test.slow", "input": {}},
                {"type": "cancel", "id": "queued"},
            ],
        )
        terminal = {m["id"]: m for m in out if m["type"] in ("result", "error")}
        self.assertEqual(terminal["first"]["type"], "result")
        self.assertEqual((terminal["queued"]["type"], terminal["queued"]["code"]), ("error", "cancelled"))
        self.assertEqual(started, ["first"])

    def test_memory_error_is_out_of_memory(self):
        worker = Worker("t", "1.0.0", ["test.oom"])

        @worker.handler("test.oom")
        def oom(ctx: Context):
            raise MemoryError()

        out = run_in_process(worker, [{"type": "request", "id": "m", "op": "test.oom", "input": {}}])
        error = [m for m in out if m["type"] == "error"][0]
        self.assertEqual((error["code"], error["retryable"]), ("out_of_memory", True))


class SubprocessTests(unittest.TestCase):
    def spawn(self, *args):
        return subprocess.Popen(
            [sys.executable, ECHO, *args],
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            bufsize=1,
        )

    def send(self, proc, message):
        proc.stdin.write(json.dumps(message) + "\n")
        proc.stdin.flush()

    def test_cancel_is_answered_promptly(self):
        proc = self.spawn()
        try:
            hello = json.loads(proc.stdout.readline())
            self.assertEqual(hello["protocol"], 1)
            self.send(proc, {"type": "request", "id": "s", "op": "test.echo", "input": {"mode": "slow", "steps": 400}})
            first = json.loads(proc.stdout.readline())
            self.assertEqual(first["type"], "progress")
            started = time.monotonic()
            self.send(proc, {"type": "cancel", "id": "s"})
            while True:
                message = json.loads(proc.stdout.readline())
                if message["type"] != "progress":
                    break
            self.assertLess(time.monotonic() - started, 2.0)
            self.assertEqual((message["type"], message["code"]), ("error", "cancelled"))
            self.send(proc, {"type": "shutdown"})
            self.assertEqual(proc.wait(timeout=5), 0)
        finally:
            if proc.poll() is None:
                proc.kill()
            proc.stdout.close()
            proc.stderr.close()
            proc.stdin.close()

    def test_stray_print_goes_to_stderr(self):
        proc = self.spawn()
        try:
            proc.stdout.readline()
            self.send(proc, {"type": "request", "id": "n", "op": "test.echo", "input": {"mode": "stdout_noise"}})
            self.send(proc, {"type": "shutdown"})
            out, err = proc.communicate(timeout=10)
            lines = [json.loads(line) for line in out.splitlines()]
            self.assertEqual(lines, [{"type": "result", "id": "n", "output": {"noise": True}}])
            self.assertIn("must not reach the protocol stream", err)
        finally:
            if proc.poll() is None:
                proc.kill()


if __name__ == "__main__":
    unittest.main()
