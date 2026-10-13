"""Tsubame worker protocol v1 — Python SDK.

JSON Lines over stdio, see ``docs/19-dubbing-pipeline.md`` ("Worker protocol v1").

* stdout carries protocol messages only. ``Worker.run()`` re-points
  ``sys.stdout`` at stderr so a stray ``print()`` in a library can never corrupt
  the protocol stream; protocol lines are written through the original stream.
* Logs go to stderr (``Context.log`` additionally emits a structured ``log``
  message the host forwards into the job log).
* Requests are handled one at a time. A reader thread consumes stdin so that a
  ``cancel`` message flips ``Context.cancelled`` while the handler is running.
  Handlers should call ``ctx.check_cancelled()`` between units of work; the host
  kills the worker if it does not answer with ``error`` code ``cancelled`` within
  2 s of the cancel.
* Workers never write SQLite and must write only inside ``input["out_dir"]``.

Written for Tsubame (MIT). No ASMR-Dubber code is ported in this module.
"""

from __future__ import annotations

import json
import os
import queue
import sys
import threading
import traceback
from dataclasses import dataclass, field
from typing import IO, Any, Callable, Dict, Iterable, List, Mapping, Optional, Union

PROTOCOL_VERSION = 1

Handler = Callable[["Context"], Mapping[str, Any]]


class WorkerError(Exception):
    """Raise from a handler to report a structured ``error`` message."""

    def __init__(self, code: str, message: str, retryable: bool = False) -> None:
        super().__init__(message)
        self.code = code
        self.message = message
        self.retryable = retryable


class Cancelled(WorkerError):
    """Raised by ``Context.check_cancelled`` once the host has sent ``cancel``."""

    def __init__(self, message: str = "cancelled by host") -> None:
        super().__init__("cancelled", message, retryable=True)


@dataclass
class Capability:
    id: str
    features: Dict[str, Any] = field(default_factory=dict)

    def to_json(self) -> Dict[str, Any]:
        return {"id": self.id, "features": dict(self.features)}


class _Channel:
    """Thread-safe writer for protocol lines."""

    def __init__(self, stream: IO[str]) -> None:
        self._stream = stream
        self._lock = threading.Lock()

    def send(self, message: Mapping[str, Any]) -> None:
        line = json.dumps(message, ensure_ascii=False, separators=(",", ":"))
        if "\n" in line:  # json.dumps never emits raw newlines; defensive.
            raise ValueError("protocol line must not contain a newline")
        with self._lock:
            self._stream.write(line + "\n")
            self._stream.flush()


class Context:
    """Per-request handle passed to a handler."""

    def __init__(self, channel: _Channel, request_id: str, op: str, payload: Dict[str, Any]) -> None:
        self._channel = channel
        self.id = request_id
        self.op = op
        self.input = payload
        self._cancel = threading.Event()

    @property
    def out_dir(self) -> Optional[str]:
        value = self.input.get("out_dir")
        return str(value) if value is not None else None

    @property
    def cancelled(self) -> bool:
        return self._cancel.is_set()

    def check_cancelled(self) -> None:
        if self._cancel.is_set():
            raise Cancelled()

    def wait_cancelled(self, timeout: float) -> bool:
        """Sleep up to ``timeout`` seconds; returns True as soon as cancel arrives."""
        return self._cancel.wait(timeout)

    def progress(self, done: int, total: int, stage: str = "") -> None:
        self._channel.send(
            {"type": "progress", "id": self.id, "stage": stage, "done": int(done), "total": int(total)}
        )

    def log(self, level: str, message: str) -> None:
        if level not in ("info", "warn", "error"):
            level = "info"
        print(f"[{level}] {message}", file=sys.stderr, flush=True)
        self._channel.send({"type": "log", "id": self.id, "level": level, "message": str(message)})


def _normalize_capabilities(
    capabilities: Union[Iterable[Union[str, Capability, Mapping[str, Any]]], Mapping[str, Mapping[str, Any]]],
) -> List[Capability]:
    if isinstance(capabilities, Mapping):
        return [Capability(str(k), dict(v or {})) for k, v in capabilities.items()]
    result: List[Capability] = []
    for item in capabilities:
        if isinstance(item, Capability):
            result.append(item)
        elif isinstance(item, str):
            result.append(Capability(item))
        else:
            result.append(Capability(str(item["id"]), dict(item.get("features") or {})))
    return result


_EOF = object()


class Worker:
    """A protocol v1 worker. Register handlers per capability id (``op``)."""

    def __init__(
        self,
        name: str,
        version: str,
        capabilities: Union[Iterable[Any], Mapping[str, Mapping[str, Any]]] = (),
        devices: Iterable[str] = ("cpu",),
        protocol: int = PROTOCOL_VERSION,
        stdin: Optional[IO[str]] = None,
        stdout: Optional[IO[str]] = None,
    ) -> None:
        self.name = name
        self.version = version
        self.capabilities = _normalize_capabilities(capabilities)
        self.devices = list(devices)
        self.protocol = protocol
        self._stdin = stdin
        self._stdout = stdout
        self._handlers: Dict[str, Handler] = {}
        self._active: Optional[Context] = None
        # Cancels that arrived before their request started running.
        self._early_cancels: set = set()
        self._active_lock = threading.Lock()

    def handler(self, op: str) -> Callable[[Handler], Handler]:
        def register(fn: Handler) -> Handler:
            self._handlers[op] = fn
            if not any(cap.id == op for cap in self.capabilities):
                self.capabilities.append(Capability(op))
            return fn

        return register

    def hello(self) -> Dict[str, Any]:
        return {
            "type": "hello",
            "protocol": self.protocol,
            "worker": self.name,
            "version": self.version,
            "capabilities": [cap.to_json() for cap in self.capabilities],
            "devices": list(self.devices),
        }

    # -- main loop ---------------------------------------------------------

    def run(self) -> int:
        stdin = self._stdin if self._stdin is not None else sys.stdin
        if self._stdout is not None:
            out = self._stdout
        else:
            out = sys.stdout
            # Protect the protocol stream from stray prints.
            sys.stdout = sys.stderr
        channel = _Channel(out)
        channel.send(self.hello())

        requests: "queue.Queue[Any]" = queue.Queue()
        reader = threading.Thread(target=self._read_loop, args=(stdin, channel, requests), daemon=True)
        reader.start()

        while True:
            item = requests.get()
            if item is _EOF:
                return 0
            self._handle(channel, item)

    def _read_loop(self, stdin: IO[str], channel: _Channel, requests: "queue.Queue[Any]") -> None:
        for raw in stdin:
            line = raw.strip()
            if not line:
                continue
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                print(f"[error] ignoring non-JSON host line: {line[:200]}", file=sys.stderr, flush=True)
                continue
            kind = message.get("type") if isinstance(message, dict) else None
            if kind == "request":
                requests.put(message)
            elif kind == "cancel":
                with self._active_lock:
                    active = self._active
                    if active is not None and active.id == message.get("id"):
                        active._cancel.set()
                    else:
                        self._early_cancels.add(str(message.get("id", "")))
            elif kind == "shutdown":
                with self._active_lock:
                    if self._active is not None:
                        self._active._cancel.set()
                break
            else:
                print(f"[warn] ignoring host message type {kind!r}", file=sys.stderr, flush=True)
        requests.put(_EOF)

    def _handle(self, channel: _Channel, message: Dict[str, Any]) -> None:
        request_id = str(message.get("id", ""))
        op = str(message.get("op", ""))
        payload = message.get("input")
        if not isinstance(payload, dict):
            payload = {}
        ctx = Context(channel, request_id, op, payload)
        handler = self._handlers.get(op)
        if handler is None:
            channel.send(
                {
                    "type": "error",
                    "id": request_id,
                    "code": "unsupported_op",
                    "retryable": False,
                    "message": f"worker {self.name} does not implement {op!r}",
                }
            )
            return
        with self._active_lock:
            self._active = ctx
            if request_id in self._early_cancels:
                self._early_cancels.discard(request_id)
                ctx._cancel.set()
        try:
            output = handler(ctx)
            if ctx.cancelled:
                raise Cancelled()
            channel.send({"type": "result", "id": request_id, "output": dict(output or {})})
        except WorkerError as exc:
            channel.send(
                {
                    "type": "error",
                    "id": request_id,
                    "code": exc.code,
                    "retryable": bool(exc.retryable),
                    "message": exc.message,
                }
            )
        except Exception as exc:  # noqa: BLE001 — every failure must become a protocol error
            traceback.print_exc(file=sys.stderr)
            channel.send(
                {
                    "type": "error",
                    "id": request_id,
                    "code": "internal",
                    "retryable": False,
                    "message": f"{type(exc).__name__}: {exc}",
                }
            )
        finally:
            with self._active_lock:
                self._active = None


def resolve_out_path(ctx: Context, name: str) -> str:
    """Join ``name`` onto ``out_dir`` and refuse anything that escapes it."""
    out_dir = ctx.out_dir
    if not out_dir:
        raise WorkerError("bad_request", "request input has no out_dir")
    base = os.path.realpath(out_dir)
    target = os.path.realpath(os.path.join(base, name))
    if os.path.commonpath([base, target]) != base:
        raise WorkerError("bad_request", f"output path escapes out_dir: {name}")
    return target
