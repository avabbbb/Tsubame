# workers/dubber/

Python workers for the PR #10 dubbing pipeline, speaking **worker protocol v1**
(JSON Lines over stdio). The contract is [docs/19-dubbing-pipeline.md](../../docs/19-dubbing-pipeline.md);
the Rust host is `app/src-tauri/src/worker.rs` and jobs are persisted by
`app/src-tauri/src/jobs.rs`.

Workers never write SQLite and never own product state. Runtime packs supply the
interpreter and packages; nothing here installs anything.

## Layout

| Path | Purpose |
|---|---|
| `tsubame_worker/` | Protocol SDK (standard library only) |
| `echo_worker.py` | Test worker used by the Rust host tests; smallest SDK example |
| `tests/` | SDK unit tests: `python3 -m unittest discover -s workers/dubber/tests -v` |
| `spatial/` | `audio.spatialize` worker (separate PR #10 track) |

## Writing a worker

```python
from tsubame_worker import Worker, WorkerError, resolve_out_path

worker = Worker("tsubame-spatial", "0.1.0", {"audio.spatialize": {"modes": ["source_trajectory"]}})

@worker.handler("audio.spatialize")
def spatialize(ctx):
    for i, clip in enumerate(ctx.input["clips"]):
        ctx.check_cancelled()                 # raises Cancelled → error code "cancelled"
        ctx.progress(i, len(ctx.input["clips"]), "render")
        out = resolve_out_path(ctx, f"{clip['id']}.wav")   # refuses paths outside out_dir
        ...
    return {"clip_paths": [...]}              # → {"type":"result",...}

raise SystemExit(worker.run())
```

- `Worker.run()` sends `hello`, then serves requests one at a time until
  `shutdown` or stdin EOF. It re-points `sys.stdout` at stderr, so stray `print()`
  calls cannot corrupt the protocol stream.
- A reader thread consumes stdin, so a `cancel` sets `ctx.cancelled` while the
  handler runs. Check it between units of work: the host kills a worker that has
  not answered with `error` code `cancelled` within 2 s.
- Raise `WorkerError(code, message, retryable)` for structured failures. Any other
  exception becomes `error` code `internal` (not retryable). Unknown ops answer
  `unsupported_op`.

## Host rules a worker must respect

- **Capabilities.** The host rejects a hello with `protocol != 1` or without the
  capability it is about to request.
- **Inputs are read-only.** The host hashes (sha256) every input file before the
  request and after it ends; any change fails the job with `input_mutated`.
- **Write only inside `input.out_dir`.** The host sets `out_dir` itself. Output
  paths are reported under keys named `path` / `*_path` (string) or `paths` /
  `*_paths` (list of strings), at any depth of `output`; relative paths resolve
  against `out_dir`. Each must resolve (symlinks included) to an existing regular
  file inside `out_dir`, else the job fails with `output_escape` /
  `output_missing`. The host then moves accepted files into the asset store.
- **stdout is protocol only.** A non-JSON stdout line, an unknown message type or
  a message for another request id is a protocol violation: the worker is killed.

## Host error codes

`worker_spawn_failed`, `worker_hello_timeout`, `worker_protocol_mismatch`,
`worker_missing_capability`, `worker_protocol_violation`, `worker_exited`,
`worker_dead`, `worker_timeout`, `cancelled`, `input_mutated`, `input_missing`,
`output_escape`, `output_missing`, `bad_request`, `worker_io`. Codes a worker
reports in its own `error` message pass through unchanged.
