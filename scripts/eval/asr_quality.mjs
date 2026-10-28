// Deterministic processing-quality checks (T10), used by scripts/eval/run.mjs.
//
//   asr_cer            transcribe every gold clip with the Faster-Whisper worker
//                      (workers/asr/faster_whisper_worker.py, same JSON contract as
//                      the Desktop) and compare with reference_transcript.
//   tts_roundtrip_cer  synthesize every reference_translation with
//                      YUZUKI_EVAL_TTS_CMD, transcribe the WAV back with the same
//                      worker, and compare with the text that was synthesized.
//
// Both report corpus CER (sum of distances / sum of reference lengths, see
// cer.mjs). A missing gold set or runtime is a skip with its reason; a broken
// manifest, a failing runtime or an unscorable clip is a fail. Nothing is
// estimated or filled in.
//
// Environment:
//   TSUBAME_FASTER_WHISPER_PYTHON  Python with faster-whisper installed (required)
//   YUZUKI_EVAL_ASR_MODEL          model id, default large-v3
//   YUZUKI_EVAL_TTS_CMD            shell command; reads {text, language, out_path}
//                                  JSON on stdin and writes a WAV to out_path
//   YUZUKI_EVAL_ASR_CER_MAX        optional gate: fail when corpus CER is above it
//   YUZUKI_EVAL_TTS_CER_MAX        optional gate for the round trip
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { cerParts } from "./cer.mjs";

const WORKER_TIMEOUT_MS = 30 * 60_000;
const AUDIO_EXT = /\.(wav|flac|mp3|m4a|ogg|opus)$/i;

/** Reads and checks fixtures/gold/manifest.json. Returns null when it does not exist. */
export function loadManifest(goldDir) {
  const path = join(goldDir, "manifest.json");
  if (!existsSync(path)) return null;
  let manifest;
  try {
    manifest = JSON.parse(readFileSync(path, "utf8"));
  } catch (error) {
    throw new Error("manifest.json is not valid JSON: " + error.message);
  }
  const problems = [];
  if (manifest?.schema_version !== 1) problems.push("schema_version must be 1");
  const clips = Array.isArray(manifest?.clips) ? manifest.clips : [];
  if (!clips.length) problems.push("clips must be a non-empty array");
  const seen = new Set();
  clips.forEach((clip, i) => {
    const where = "clips[" + i + "]" + (clip?.id ? " (" + clip.id + ")" : "");
    for (const key of ["id", "audio", "reference_transcript", "reference_translation"]) {
      if (typeof clip?.[key] !== "string" || !clip[key].trim()) problems.push(where + ": " + key + " is required");
    }
    if (clip?.language !== "ja") problems.push(where + ": language must be ja");
    if (clip?.rights?.evaluation_permitted !== true) problems.push(where + ": rights.evaluation_permitted must be true");
    if (typeof clip?.id === "string") {
      if (seen.has(clip.id)) problems.push(where + ": duplicate id");
      seen.add(clip.id);
    }
    if (typeof clip?.audio === "string") {
      if (!AUDIO_EXT.test(clip.audio) || clip.audio.includes("..") || /^[\\/]|:/.test(clip.audio)) {
        problems.push(where + ": audio must be a relative audio path inside fixtures/gold");
      } else if (!existsSync(join(goldDir, clip.audio))) {
        problems.push(where + ": audio file not found: " + clip.audio);
      }
    }
  });
  if (problems.length) throw new Error("manifest.json: " + problems.join("; "));
  return { clips, translationLanguage: manifest.translation_language ?? null };
}

/** BCP 47 tag to the language code faster-whisper expects (zh-Hans -> zh). */
export function asrLanguage(tag) {
  return String(tag).split("-")[0].toLowerCase();
}

/** Runs the Faster-Whisper worker on one file and returns the transcript text. */
export function transcribeWithWorker(mediaPath, { python, worker, model, language }) {
  const request = {
    contract_version: 1,
    adapter_id: "faster-whisper",
    media_path: mediaPath,
    media_duration_ms: 0,
    model_id: model,
    language,
  };
  const r = spawnSync(python, [worker], {
    input: JSON.stringify(request),
    encoding: "utf8",
    timeout: WORKER_TIMEOUT_MS,
    maxBuffer: 64 << 20,
    windowsHide: true,
  });
  if (r.error) throw new Error("ASR worker did not start: " + r.error.message);
  if (r.status !== 0) throw new Error("ASR worker failed: " + String(r.stderr ?? "").trim().split(/\r?\n/).slice(-3).join(" | "));
  let result;
  try {
    result = JSON.parse(r.stdout);
  } catch (error) {
    throw new Error("invalid ASR worker JSON: " + error.message);
  }
  if (result?.contract_version !== 1 || !Array.isArray(result.segments)) throw new Error("unexpected ASR worker result");
  return result.segments.map((s) => s.text).join("\n");
}

/** Runs YUZUKI_EVAL_TTS_CMD for one text; the command must write out_path. */
export function synthesizeWithCommand(cmd, { text, language, outPath }) {
  const r = spawnSync(cmd, {
    shell: true,
    input: JSON.stringify({ text, language, out_path: outPath }),
    encoding: "utf8",
    timeout: WORKER_TIMEOUT_MS,
    maxBuffer: 64 << 20,
    windowsHide: true,
  });
  if (r.error) throw new Error("TTS command did not start: " + r.error.message);
  if (r.status !== 0) throw new Error("TTS command failed: " + String(r.stderr ?? "").trim().split(/\r?\n/).slice(-3).join(" | "));
  if (!existsSync(outPath)) throw new Error("TTS command exited 0 but wrote no file");
}

/** Corpus CER over clips; hypothesis(clip) returns the ASR text or throws. */
export function scoreClips(clips, reference, hypothesis) {
  let distance = 0;
  let refLen = 0;
  const perClip = [];
  const errors = [];
  for (const clip of clips) {
    try {
      const p = cerParts(reference(clip), hypothesis(clip));
      distance += p.distance;
      refLen += p.refLen;
      perClip.push({ id: clip.id, cer: +(p.distance / p.refLen).toFixed(4), distance: p.distance, ref_len: p.refLen });
    } catch (error) {
      errors.push(clip.id + ": " + error.message);
    }
  }
  return { cer: refLen ? distance / refLen : null, distance, refLen, perClip, errors };
}

function verdict(score, maxEnv, env, label) {
  const metrics = {
    cer: score.cer == null ? null : +score.cer.toFixed(4),
    clips: score.perClip.length,
    distance: score.distance,
    ref_len: score.refLen,
    per_clip: score.perClip,
  };
  if (score.errors.length) {
    return { status: "fail", detail: score.errors.length + " clip(s) not scored:\n" + score.errors.slice(0, 10).join("\n"), metrics };
  }
  const raw = env[maxEnv];
  const max = raw == null || raw === "" ? null : Number(raw);
  if (max != null && !Number.isFinite(max)) return { status: "fail", detail: maxEnv + " is not a number: " + raw, metrics };
  const summary = label + " CER " + metrics.cer.toFixed(4) + " on " + metrics.clips + " clips (" + score.refLen + " ref chars)";
  if (max == null) return { status: "pass", detail: summary + "; no gate (set " + maxEnv + ")", metrics };
  metrics.max = max;
  return metrics.cer <= max
    ? { status: "pass", detail: summary + " <= " + max, metrics }
    : { status: "fail", detail: summary + " > " + max, metrics };
}

function prepare(goldDir, env) {
  let gold;
  try {
    gold = loadManifest(goldDir);
  } catch (error) {
    return { result: { status: "fail", detail: error.message } };
  }
  if (!gold) return { result: { status: "skip", detail: "no fixtures/gold/manifest.json (see fixtures/gold/README.md)" } };
  if (!env.TSUBAME_FASTER_WHISPER_PYTHON) {
    return { result: { status: "skip", detail: "ASR runtime not configured: set TSUBAME_FASTER_WHISPER_PYTHON" } };
  }
  return { gold };
}

/**
 * ASR CER on the gold set. options: goldDir, worker (path to
 * faster_whisper_worker.py), env, transcribe (injectable for tests).
 */
export function asrCerCheck({ goldDir, worker, env = process.env, transcribe = transcribeWithWorker }) {
  const { gold, result } = prepare(goldDir, env);
  if (result) return result;
  const runtime = { python: env.TSUBAME_FASTER_WHISPER_PYTHON, worker, model: env.YUZUKI_EVAL_ASR_MODEL || "large-v3" };
  const score = scoreClips(
    gold.clips,
    (clip) => clip.reference_transcript,
    (clip) => transcribe(join(goldDir, clip.audio), { ...runtime, language: "ja" }),
  );
  return verdict(score, "YUZUKI_EVAL_ASR_CER_MAX", env, "ASR");
}

/**
 * TTS round-trip CER: reference_translation -> TTS -> ASR -> CER against the
 * synthesized text. options: goldDir, worker, outDir (WAVs are written here),
 * env, transcribe and synthesize (injectable for tests).
 */
export function ttsRoundtripCheck({
  goldDir,
  worker,
  outDir,
  env = process.env,
  transcribe = transcribeWithWorker,
  synthesize = synthesizeWithCommand,
}) {
  const { gold, result } = prepare(goldDir, env);
  if (result) return result;
  if (!env.YUZUKI_EVAL_TTS_CMD) return { status: "skip", detail: "TTS not configured: set YUZUKI_EVAL_TTS_CMD" };
  if (!gold.translationLanguage) {
    return { status: "fail", detail: "manifest.json: translation_language is required for the TTS round trip" };
  }
  mkdirSync(outDir, { recursive: true });
  const runtime = { python: env.TSUBAME_FASTER_WHISPER_PYTHON, worker, model: env.YUZUKI_EVAL_ASR_MODEL || "large-v3" };
  const language = gold.translationLanguage;
  const score = scoreClips(
    gold.clips,
    (clip) => clip.reference_translation,
    (clip) => {
      const outPath = join(outDir, clip.id + ".wav");
      synthesize(env.YUZUKI_EVAL_TTS_CMD, { text: clip.reference_translation, language, outPath });
      return transcribe(outPath, { ...runtime, language: asrLanguage(language) });
    },
  );
  return verdict(score, "YUZUKI_EVAL_TTS_CER_MAX", env, "TTS round-trip");
}
