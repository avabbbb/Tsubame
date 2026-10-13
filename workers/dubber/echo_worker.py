#!/usr/bin/env python3
"""Tsubame protocol v1 test worker.

Used by the Rust worker-host tests (``app/src-tauri/src/worker.rs``) and as the
smallest working example of the ``tsubame_worker`` SDK. It has no third-party
dependencies.

Command line (to simulate misbehaving workers):
  --protocol N         announce protocol N in hello (default 1)
  --capability ID      announce capability ID (repeatable; default ``test.echo``)
  --hello-delay SEC    sleep before sending hello
  --no-hello           never send hello (sleeps until killed)

Request ``input.mode`` selects the behaviour of op ``test.echo``:
  echo (default)  result ``{"echo": input}``
  progress        ``steps`` progress messages, then result
  log             one structured log + one raw stderr line, then result
  error           error with ``code``/``retryable``/``message`` from input
  slow            cooperative loop of ``steps`` x 50 ms; honours cancel
  ignore_cancel   sleeps ``sleep_s`` (default 30) ignoring cancel
  mutate_input    appends a byte to ``input.input_path``, then result
  write           writes ``name`` (default out.txt) in out_dir, result {"path": name}
  escape          returns ``{"path": input.escape_path}`` (a path outside out_dir)
  stdout_noise    ``print()``s to stdout (SDK must divert it to stderr), then result
  crash           exits the process with status 3 without answering
"""

from __future__ import annotations

import argparse
import os
import sys
import time

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from tsubame_worker import Context, Worker, WorkerError, resolve_out_path  # noqa: E402


def build(args: argparse.Namespace) -> Worker:
    capabilities = args.capability or ["test.echo"]
    worker = Worker(
        name="tsubame-echo",
        version="0.1.0",
        capabilities={cap: {"modes": "all"} for cap in capabilities},
        protocol=args.protocol,
    )

    def echo(ctx: Context):
        mode = ctx.input.get("mode", "echo")
        steps = int(ctx.input.get("steps", 3))
        if mode == "echo":
            return {"echo": ctx.input}
        if mode == "progress":
            for i in range(1, steps + 1):
                ctx.progress(i, steps, "echo")
            return {"steps": steps}
        if mode == "log":
            ctx.log("info", "hello from the echo worker")
            print("raw stderr line", file=sys.stderr, flush=True)
            return {}
        if mode == "error":
            raise WorkerError(
                str(ctx.input.get("code", "boom")),
                str(ctx.input.get("message", "requested failure")),
                bool(ctx.input.get("retryable", False)),
            )
        if mode == "slow":
            for i in range(steps):
                ctx.check_cancelled()
                ctx.progress(i, steps, "slow")
                ctx.wait_cancelled(0.05)
            ctx.check_cancelled()
            return {"steps": steps}
        if mode == "ignore_cancel":
            time.sleep(float(ctx.input.get("sleep_s", 30)))
            return {}
        if mode == "mutate_input":
            with open(ctx.input["input_path"], "ab") as handle:
                handle.write(b"!")
            return {}
        if mode == "write":
            name = str(ctx.input.get("name", "out.txt"))
            with open(resolve_out_path(ctx, name), "w", encoding="utf-8") as handle:
                handle.write("ok")
            return {"path": name}
        if mode == "escape":
            return {"path": ctx.input["escape_path"]}
        if mode == "stdout_noise":
            print("this must not reach the protocol stream")
            return {"noise": True}
        if mode == "crash":
            sys.stderr.flush()
            os._exit(3)
        raise WorkerError("bad_request", f"unknown mode {mode!r}")

    for cap in capabilities:
        worker.handler(cap)(echo)
    return worker


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--protocol", type=int, default=1)
    parser.add_argument("--capability", action="append")
    parser.add_argument("--hello-delay", type=float, default=0.0)
    parser.add_argument("--no-hello", action="store_true")
    args = parser.parse_args()
    if args.no_hello:
        time.sleep(3600)
        return 0
    if args.hello_delay:
        time.sleep(args.hello_delay)
    return build(args).run()


if __name__ == "__main__":
    raise SystemExit(main())
