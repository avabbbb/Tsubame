"""Development-path render + mix for a dub run directory (not used by the Desktop).

Usage: python dev_render_mix.py RUN_DIR REPO_ROOT source_trajectory
RUN_DIR holds source_stereo.wav, one .mp4, spatial/spatial_track.npz and tts_manifest.json.
Writes RUN_DIR/mix/{dub_stem.wav, bilingual_mix.wav, bilingual.mp4, mix_report.json}.
"""
import sys, json, time, math, subprocess, pathlib
import numpy as np
run = pathlib.Path(sys.argv[1]); repo = pathlib.Path(sys.argv[2]); mode = sys.argv[3]
sys.path.insert(0, str(repo / "workers" / "dubber"))
from spatial._wav import read as wav_read, write_float32, WavReader
from spatial.analyze import SpatialTrack
from spatial.render import render_array, resample
t0 = time.time()
sp = run / "spatial"; track = None; last = None
for cand in (sp / "spatial_track.npz", sp / "spatial_track.json", sp):
    try:
        track = SpatialTrack.load(cand); break
    except Exception as e:
        last = repr(e)
assert track is not None, last
ref_path = run / "source_stereo.wav"
orig, sr = wav_read(ref_path)
orig = orig.astype(np.float32); n = len(orig)
dub = np.zeros((n, 2), np.float32); cover = np.zeros(n, np.int8)
man = json.loads((run / "tts_manifest.json").read_text(encoding="utf-8"))
reps = []
def samples(sec, rate):
    return int(round(np.multiply(max(0.0, sec), rate)))
with WavReader(ref_path) as ref:
    for x in man["items"]:
        if "file" not in x: continue
        a, r = wav_read(run / x["file"])
        m = a.mean(axis=1).astype(np.float64)
        lead = samples(x.get("lead_s", 0) - 0.02, r); tail = samples(x.get("tail_s", 0) - 0.03, r)
        m = m[lead: len(m) - tail] if len(m) - tail > lead else m
        m = resample(m, r, sr)
        f = sr // 200; ramp = np.linspace(0.0, 1.0, f)
        m[:f] = np.multiply(m[:f], ramp); m[-f:] = np.multiply(m[-f:], ramp[::-1])
        out, rep = render_array(m, sr, ref, track, mode=mode, window={"src_start_ms": x["start_ms"], "src_end_ms": x["end_ms"], "dub_start_ms": x["start_ms"]})
        s = samples(x["start_ms"] / 1000, sr); e = min(n, s + len(out))
        dub[s:e] += out[: e - s]; cover[s:e] += 1
        reps.append({"i": x["i"], "fallback": rep["fallback_blocks"], "conf": rep["mean_rtf_confidence"], "held": rep["held_frame_fraction"], "len_s": round(len(out) / sr, 3)})
render_s = round(time.time() - t0, 1)
active = np.abs(dub).max(axis=1) > 1e-4
win = sr // 7
env = np.convolve(active.astype(np.float32), np.ones(win, np.float32) / win, mode="same")
env = np.minimum(1.0, np.multiply(env, 3.0))
duck_db = -9.0; g = math.pow(10.0, duck_db / 20)
orig_gain = 1.0 - np.multiply(env, 1.0 - g)
def rms(v): return float(np.sqrt(np.mean(np.square(v)))) if v.size else 0.0
o_rms = rms(orig[active]); d_rms = rms(dub[active])
dub_gain = o_rms / max(d_rms, 1e-9)
stem = np.multiply(dub, dub_gain).astype(np.float32)
mix = np.multiply(orig, orig_gain[:, None]) + stem
peak = float(np.abs(mix).max()); PEAK_CEIL = 0.85  # about -1.4 dBFS sample peak, leaves headroom for AAC inter-sample overs
norm = PEAK_CEIL / peak if peak > PEAK_CEIL else 1.0
mix = np.multiply(mix, norm).astype(np.float32); stem = np.multiply(stem, norm).astype(np.float32)
out_dir = run / "mix"; out_dir.mkdir(exist_ok=True)
write_float32(out_dir / "dub_stem.wav", stem, sr); write_float32(out_dir / "bilingual_mix.wav", mix, sr)
hop = int(track.meta.get("hop_samples", 882)) if hasattr(track, "meta") else 882
fi = np.minimum(np.flatnonzero(active) // hop, len(track.active) - 1)
overlap_orig = float(np.mean(track.active[fi])) if fi.size else 0.0
mp4 = sorted(p for p in run.iterdir() if p.suffix.lower() == ".mp4")[0]
cmd = ["ffmpeg", "-y", "-hide_banner", "-loglevel", "error", "-i", str(mp4), "-i", str(out_dir / "bilingual_mix.wav"), "-map", "0:v?", "-map", "1:a", "-map", "0:a:0", "-c:v", "copy", "-c:a", "aac", "-b:a", "256k", "-metadata:s:a:0", "language=chi", "-metadata:s:a:0", "title=ZH dub + JA original", "-metadata:s:a:1", "language=jpn", "-metadata:s:a:1", "title=JA original", "-disposition:a:0", "default", "-disposition:a:1", "0", str(out_dir / "bilingual.mp4")]
t1 = time.time(); ff = subprocess.run(cmd, capture_output=True, text=True); mux_s = round(time.time() - t1, 1)
fb = {}
for r_ in reps:
    for k, v in r_["fallback"].items(): fb[k] = fb.get(k, 0) + v
confs = [r_["conf"] for r_ in reps if r_["conf"] is not None]
rep = {"mode": mode, "rendered": len(reps), "render_s": render_s, "total_s": round(time.time() - t0, 1), "mux_s": mux_s, "mux_rc": ff.returncode, "mux_err": ff.stderr[-300:],
       "duck_db": duck_db, "dub_gain_db": round(float(np.multiply(20, math.log10(dub_gain))), 2), "peak_before_guard": round(peak, 3), "guard_gain_db": round(float(np.multiply(20, math.log10(norm))), 2),
       "dub_active_s": round(active.sum() / sr, 1), "dub_dub_overlap_s": round(float((cover > 1).sum()) / sr, 2), "dub_over_active_original": round(overlap_orig, 3),
       "fallback_blocks": fb, "mean_rtf_conf": round(float(np.mean(confs)), 3) if confs else None, "clips_with_conf": len(confs), "items": reps}
(out_dir / "mix_report.json").write_text(json.dumps(rep, ensure_ascii=False, indent=1), encoding="utf-8")
print(json.dumps({k: v for k, v in rep.items() if k != "items"}, ensure_ascii=False))
for f_ in sorted(out_dir.iterdir()): print("out", f_.name, f_.stat().st_size)
