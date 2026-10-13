"""Private minimal implementation of Tsubame worker protocol v1 (JSON Lines/stdio).

See ``docs/19-dubbing-pipeline.md`` § "Worker protocol v1". This module exists so
the spatial worker could be built in parallel with the shared SDK
(``workers/dubber/tsubame_worker/``); the integrator is expected to replace it with
that SDK. It must stay wire-identical to the contract:

- stdout carries protocol messages only, one JSON object per line; logs → stderr.
- The first line is ``hello``.
- Requests run one at a time in a background thread, in arrival order, so the
  stdin reader stays responsive to ``cancel`` and ``shutdown``.
- ``cancel`` for the running or a queued job is answered immediately with
  ``error`` code ``cancelled`` (well inside the 2 s budget); the job's thread
  stops cooperatively at its next ``check_cancel()`` and any late output for that
  id is suppressed. ``cancel`` for an unknown/finished id is ignored (stderr note).
- ``shutdown`` cancels outstanding work (``error`` ``cancelled``, retryable) and
  exits 0 within ~2 s.
- stdin EOF without ``shutdown`` drains: queued jobs finish, then exit 0. This
  keeps ``worker.py < requests.jsonl`` usable from scripts/CLI.
"""

from __future__ import annotations

import json
import queue
import sys
import threading
import traceback
from dataclasses import dataclass, field
from typing import Callable

PROTOCOL_VERSION = 1


class WorkerError(Exception):
    def __init__(self, code: str, message: str, retryable: bool = False):
        super().__init__(message)
        self.code = code
        self.message = message
        self.retryable = retryable


class Cancelled(Exception):
    pass


@dataclass
class Job:
    id: str
    op: str
    input: dict
    cancel: threading.Event = field(default_factory=threading.Event)
    finished: bool = False  # a terminal message (result/error) was written


class JobContext:
    def __init__(self, worker: "Worker", job: Job):
        self._w, self._job = worker, job

    @property
    def id(self) -> str:
        return self._job.id

    def check_cancel(self) -> None:
        if self._job.cancel.is_set():
            raise Cancelled()

    def progress(self, stage: str, done: int, total: int) -> None:
        self._w._emit_for(self._job, {"type": "progress", "id": self._job.id, "stage": stage,
                                      "done": int(done), "total": int(total)})

    def log(self, level: str, message: str) -> None:
        self._w._emit_for(self._job, {"type": "log", "id": self._job.id, "level": level,
                                      "message": message})


Handler = Callable[[JobContext, str, dict], dict]


class Worker:
    def __init__(self, name: str, version: str, capabilities: list[dict], handler: Handler,
                 devices: list[str] | None = None, stdin=None, stdout=None):
        self.name, self.version = name, version
        self.capabilities = capabilities
        self.devices = devices or ["cpu"]
        self.handler = handler
        self._in = stdin or sys.stdin
        self._out = stdout or sys.stdout
        self._lock = threading.Lock()
        self._queue: "queue.Queue[Job | None]" = queue.Queue()
        self._jobs: dict[str, Job] = {}
        self._cap_ids = {c["id"] for c in capabilities}

    # -- output ---------------------------------------------------------------
    def emit(self, message: dict) -> None:
        line = json.dumps(message, ensure_ascii=False, separators=(",", ":"))
        with self._lock:
            self._out.write(line + "\n")
            self._out.flush()

    def _emit_for(self, job: Job, message: dict, terminal: bool = False) -> None:
        with self._lock:
            if job.finished:
                return
            if terminal:
                job.finished = True
            line = json.dumps(message, ensure_ascii=False, separators=(",", ":"))
            self._out.write(line + "\n")
            self._out.flush()

    def _error(self, job: Job, code: str, message: str, retryable: bool) -> None:
        self._emit_for(job, {"type": "error", "id": job.id, "code": code,
                             "retryable": bool(retryable), "message": message}, terminal=True)

    # -- execution ------------------------------------------------------------
    def _run(self) -> None:
        while True:
            job = self._queue.get()
            if job is None:
                return
            try:
                if job.cancel.is_set():
                    continue
                if job.op not in self._cap_ids:
                    raise WorkerError("unsupported_op", f"worker does not provide {job.op!r}")
                output = self.handler(JobContext(self, job), job.op, job.input)
                if job.cancel.is_set():
                    raise Cancelled()
                self._emit_for(job, {"type": "result", "id": job.id, "output": output}, terminal=True)
            except Cancelled:
                self._error(job, "cancelled", "job cancelled", False)
            except WorkerError as exc:
                self._error(job, exc.code, exc.message, exc.retryable)
            except MemoryError:
                self._error(job, "out_of_memory", "worker ran out of memory", True)
            except Exception as exc:  # noqa: BLE001 - must always answer the host
                traceback.print_exc(file=sys.stderr)
                self._error(job, "internal", f"{type(exc).__name__}: {exc}", False)
            finally:
                with self._lock:
                    self._jobs.pop(job.id, None)

    def serve(self) -> int:
        self.emit({"type": "hello", "protocol": PROTOCOL_VERSION, "worker": self.name,
                   "version": self.version, "capabilities": self.capabilities,
                   "devices": self.devices})
        runner = threading.Thread(target=self._run, name="job-runner", daemon=True)
        runner.start()
        shutdown = False
        for raw in self._in:
            raw = raw.strip()
            if not raw:
                continue
            try:
                msg = json.loads(raw)
                if not isinstance(msg, dict):
                    raise ValueError("message is not an object")
            except ValueError as exc:
                print(f"[{self.name}] ignoring malformed line: {exc}", file=sys.stderr)
                continue
            kind = msg.get("type")
            if kind == "shutdown":
                shutdown = True
                break
            if kind == "cancel":
                with self._lock:
                    job = self._jobs.get(str(msg.get("id")))
                if job is None:
                    print(f"[{self.name}] cancel for unknown id {msg.get('id')!r}", file=sys.stderr)
                    continue
                job.cancel.set()
                self._error(job, "cancelled", "job cancelled", False)
                continue
            if kind != "request":
                print(f"[{self.name}] ignoring message type {kind!r}", file=sys.stderr)
                continue
            jid = msg.get("id")
            if not isinstance(jid, str) or not jid:
                print(f"[{self.name}] request without string id", file=sys.stderr)
                continue
            job = Job(jid, str(msg.get("op")), msg.get("input"))
            with self._lock:
                duplicate = jid in self._jobs
                if not duplicate:
                    self._jobs[jid] = job
            if duplicate:
                self.emit({"type": "error", "id": jid, "code": "invalid_request",
                           "retryable": False, "message": "duplicate job id in flight"})
                continue
            if not isinstance(job.input, dict):
                self._error(job, "invalid_input", "request.input must be an object", False)
                with self._lock:
                    self._jobs.pop(jid, None)
                continue
            self._queue.put(job)
        self._queue.put(None)
        if not shutdown:  # EOF: drain queued work
            runner.join()
            return 0
        # shutdown: cancel everything outstanding, give the runner a moment.
        with self._lock:
            pending = list(self._jobs.values())
        for job in pending:
            job.cancel.set()
            self._error(job, "cancelled", "worker shutting down", True)
        runner.join(timeout=2.0)
        return 0
