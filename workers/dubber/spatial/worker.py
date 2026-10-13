#!/usr/bin/env python3
"""Tsubame ``audio.spatialize`` worker: protocol v1 over stdio.

Run as ``python3 workers/dubber/spatial/worker.py`` (or ``python3 -m spatial.worker``
from ``workers/dubber``). Request ``op`` is the capability ``audio.spatialize``; the
sub-operation is ``input.op``: ``"analyze"`` (per-media track) or ``"render"``
(per-clip). See README.md for full input/output schemas.

Portions adapted from ASMR-Dubber (MIT), Copyright (c) 2026 ASMR Dubber
contributors, https://github.com/EveningStudy/asmr-dubber commit 2faea94f.
"""

from __future__ import annotations

import hashlib
import importlib.util
import json
import os
import re
import sys
import uuid
from pathlib import Path

if __package__ in (None, ""):
    sys.path.insert(0, str(Path(__file__).resolve().parent.parent))
    __package__ = "spatial"  # noqa: A001 - allow relative imports when run as a script

from . import ANALYZER_VERSION, RENDERER_VERSION, WORKER_NAME, WORKER_VERSION  # noqa: E402
from ._protocol import JobContext, Worker, WorkerError  # noqa: E402

CAPABILITY = "audio.spatialize"
_HEX64 = re.compile(r"^[0-9a-f]{64}$")
TRACK_FILE = "spatial_track.npz"
SUMMARY_FILE = "spatial_track.json"
DEFAULT_CLIP_NAME = "spatial_clip.wav"


def capabilities() -> list[dict]:
    from .render import MODES, DEFAULT_STRENGTHS

    return [{
        "id": CAPABILITY,
        "features": {
            "ops": ["analyze", "render"],
            "modes": list(MODES),
            "default_mode": "source_trajectory",
            "default_strengths": DEFAULT_STRENGTHS,
            "analyzer_version": ANALYZER_VERSION,
            "renderer_version": RENDERER_VERSION,
            "reference_channels": [2],
            "clip_channels": [1, 2],
            "formats": ["wav"],
        },
    }]


# -- input validation ------------------------------------------------------


def _str(inp: dict, key: str, required: bool = True) -> str | None:
    v = inp.get(key)
    if v is None and not required:
        return None
    if not isinstance(v, str) or not v:
        raise WorkerError("invalid_input", f"input.{key} must be a non-empty string")
    return v


def _file(inp: dict, key: str) -> Path:
    p = Path(_str(inp, key))
    if not p.is_absolute():
        raise WorkerError("invalid_input", f"input.{key} must be an absolute path")
    if not p.is_file():
        raise WorkerError("input_missing", f"input.{key} does not exist: {p}")
    return p


def _out_dir(inp: dict) -> Path:
    p = Path(_str(inp, "out_dir"))
    if not p.is_absolute() or not p.is_dir():
        raise WorkerError("invalid_input", "input.out_dir must be an existing absolute directory")
    return p


def _atomic_write(path: Path, writer) -> None:
    tmp = path.with_name(f".{uuid.uuid4().hex}.tmp")
    try:
        writer(tmp)
        os.replace(tmp, path)
    finally:
        if tmp.exists():
            tmp.unlink()


def _sha256_file(path: Path, ctx: JobContext) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for block in iter(lambda: f.read(1 << 22), b""):
            ctx.check_cancel()
            h.update(block)
    return h.hexdigest()


# -- ops -------------------------------------------------------------------


def op_analyze(ctx: JobContext, inp: dict) -> dict:
    from ._wav import WavError
    from .analyze import AnalysisError, analyze_file, resolve_params, spatial_track_key

    ref = _file(inp, "reference_path")
    kind = _str(inp, "reference_kind")
    if kind not in ("mix", "vocals"):
        raise WorkerError("invalid_input", "input.reference_kind must be 'mix' or 'vocals'")
    out_dir = _out_dir(inp)
    params = inp.get("params") or {}
    if not isinstance(params, dict):
        raise WorkerError("invalid_input", "input.params must be an object")
    try:
        resolved = resolve_params(params)
    except AnalysisError as exc:
        raise WorkerError(exc.code, str(exc)) from exc
    sha = inp.get("reference_sha256")
    if sha is None:
        ctx.log("warn", "reference_sha256 not supplied; hashing reference in worker")
        sha = _sha256_file(ref, ctx)
    elif not isinstance(sha, str) or not _HEX64.match(sha.lower()):
        raise WorkerError("invalid_input", "input.reference_sha256 must be 64 hex chars")
    sha = sha.lower()
    key = spatial_track_key(sha, kind, resolved)
    try:
        track = analyze_file(ref, resolved, progress=ctx.progress, check_cancel=ctx.check_cancel)
    except AnalysisError as exc:
        raise WorkerError(exc.code, str(exc)) from exc
    except WavError as exc:
        raise WorkerError("unsupported_format", str(exc)) from exc
    ctx.check_cancel()
    track.meta.update(spatial_track_key=key, reference_sha256=sha, reference_kind=kind)
    summary = track.summary()
    ctx.progress("write", 0, 2)
    track_path, summary_path = out_dir / TRACK_FILE, out_dir / SUMMARY_FILE
    _atomic_write(track_path, track.save)
    doc = {"spatial_track_key": key, "analyzer_version": ANALYZER_VERSION,
           "reference_sha256": sha, "reference_kind": kind, "params": resolved,
           "sample_rate": track.sample_rate, "hop_samples": track.hop,
           "frame_size": track.frame_size, "summary": summary}
    _atomic_write(summary_path, lambda p: Path(p).write_text(
        json.dumps(doc, ensure_ascii=False, indent=2), encoding="utf-8"))
    ctx.progress("write", 2, 2)
    return {
        "op": "analyze",
        "spatial_track_key": key,
        "analyzer_version": ANALYZER_VERSION,
        "track_path": str(track_path),
        "summary_path": str(summary_path),
        "frames": int(len(track.times)),
        "hop_ms": track.params["hop_ms"],
        "sample_rate": track.sample_rate,
        "duration_ms": round(track.meta["duration_seconds"] * 1000),
        "summary": summary,
    }


def _window(inp: dict, mode: str) -> dict:
    w = inp.get("window")
    if not isinstance(w, dict):
        raise WorkerError("invalid_input", "input.window must be an object")
    keys = ["src_start_ms", "src_end_ms"] + (["dub_start_ms"] if mode == "concurrent" else [])
    out = {}
    for k in keys:
        v = w.get(k)
        if not isinstance(v, int) or isinstance(v, bool) or v < 0:
            raise WorkerError("invalid_input", f"window.{k} must be a non-negative integer (ms)")
        out[k] = v
    if out["src_end_ms"] <= out["src_start_ms"]:
        raise WorkerError("invalid_input", "window.src_end_ms must be > src_start_ms")
    return out


def op_render(ctx: JobContext, inp: dict) -> dict:
    from ._wav import WavError
    from .analyze import AnalysisError, SpatialTrack
    from .render import MODES, RenderError, render_file, render_key, resolve_strengths

    clip = _file(inp, "clip_path")
    ref = _file(inp, "reference_path")
    track_path = _file(inp, "spatial_track_path")
    out_dir = _out_dir(inp)
    mode = inp.get("mode", "source_trajectory")
    if mode not in MODES:
        raise WorkerError("invalid_input", f"input.mode must be one of {list(MODES)}")
    window = _window(inp, mode)
    try:
        strengths = resolve_strengths(inp.get("strengths"))
    except RenderError as exc:
        raise WorkerError(exc.code, str(exc)) from exc
    name = inp.get("output_name", DEFAULT_CLIP_NAME)
    if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9._-]+\.wav", name) or name.startswith("."):
        raise WorkerError("invalid_input", "input.output_name must be a plain *.wav file name")
    try:
        track = SpatialTrack.load(track_path)
    except AnalysisError as exc:
        raise WorkerError(exc.code, str(exc)) from exc
    except (OSError, ValueError, KeyError) as exc:
        raise WorkerError("track_invalid", f"cannot read spatial track: {exc}") from exc
    track_key = _str(inp, "spatial_track_key", required=False)
    if track_key and track.meta.get("spatial_track_key") not in (None, track_key):
        raise WorkerError("track_mismatch", "spatial_track_key does not match the track file")
    ref_sha = _str(inp, "reference_sha256", required=False)
    if ref_sha and track.meta.get("reference_sha256") not in (None, ref_sha.lower()):
        raise WorkerError("track_mismatch", "reference_sha256 does not match the track file")
    tts_key = _str(inp, "tts_asset_key", required=False)
    key = (render_key(tts_key, track_key or track.meta.get("spatial_track_key", ""), mode, window, strengths)
           if tts_key and (track_key or track.meta.get("spatial_track_key")) else None)
    out_path = out_dir / name
    try:
        report = render_file(clip, ref, track, out_path, mode=mode, window=window,
                             strengths=strengths, progress=ctx.progress,
                             check_cancel=ctx.check_cancel)
    except RenderError as exc:
        raise WorkerError(exc.code, str(exc)) from exc
    except WavError as exc:
        raise WorkerError("unsupported_format", str(exc)) from exc
    for reason, count in report["fallback_blocks"].items():
        ctx.log("warn", f"spatial fallback '{reason}' used for {count} block(s)")
    return {
        "op": "render",
        "path": str(out_path),
        "render_key": key,
        "sample_rate": report.pop("sample_rate"),
        "channels": report.pop("channels"),
        "frames": report.pop("frames"),
        "duration_ms": report.pop("duration_ms"),
        "report": report,
    }


def handle(ctx: JobContext, op: str, inp: dict) -> dict:
    sub = inp.get("op")
    if sub == "analyze":
        return op_analyze(ctx, inp)
    if sub == "render":
        return op_render(ctx, inp)
    raise WorkerError("unsupported_op", "input.op must be 'analyze' or 'render'")


def main() -> int:
    if importlib.util.find_spec("numpy") is None:  # runtime pack incomplete
        print(f"[{WORKER_NAME}] numpy unavailable", file=sys.stderr)

        def handle_missing(ctx, op, inp):
            raise WorkerError("runtime_missing", "numpy is not installed in this runtime", False)

        return Worker(WORKER_NAME, WORKER_VERSION, [{"id": CAPABILITY, "features": {}}],
                      handle_missing).serve()
    return Worker(WORKER_NAME, WORKER_VERSION, capabilities(), handle).serve()


if __name__ == "__main__":
    raise SystemExit(main())
