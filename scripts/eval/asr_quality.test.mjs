// T10 checks with injected transcribe/synthesize. Run: node --test scripts/eval/
import { test } from "node:test";
import assert from "node:assert/strict";
import { mkdtempSync, mkdirSync, writeFileSync, existsSync } from "node:fs";
import { tmpdir } from "node:os";
import { basename, join } from "node:path";
import { asrCerCheck, asrLanguage, loadManifest, ttsRoundtripCheck } from "./asr_quality.mjs";

const RIGHTS = { license: "user-owned", source: "test", evaluation_permitted: true, real_person_voice: "self" };
const ENV = { TSUBAME_FASTER_WHISPER_PYTHON: "python" };

function gold(clips, extra = { translation_language: "zh-Hans" }) {
  const dir = mkdtempSync(join(tmpdir(), "yuzuki-gold-"));
  mkdirSync(join(dir, "audio"));
  for (const c of clips) writeFileSync(join(dir, c.audio), "RIFF");
  writeFileSync(join(dir, "manifest.json"), JSON.stringify({ schema_version: 1, ...extra, clips }));
  return dir;
}

const clip = (id, transcript, translation) => ({
  id,
  audio: "audio/" + id + ".wav",
  language: "ja",
  reference_transcript: transcript,
  reference_translation: translation,
  rights: RIGHTS,
});

test("no manifest is a skip, not a score", () => {
  const dir = mkdtempSync(join(tmpdir(), "yuzuki-gold-"));
  assert.equal(loadManifest(dir), null);
  const r = asrCerCheck({ goldDir: dir, worker: "w.py", env: ENV, transcribe: () => assert.fail("must not run") });
  assert.equal(r.status, "skip");
  assert.match(r.detail, /manifest\.json/);
});

test("missing ASR runtime is a skip", () => {
  const dir = gold([clip("a", "おやすみ", "晚安")]);
  const r = asrCerCheck({ goldDir: dir, worker: "w.py", env: {}, transcribe: () => assert.fail("must not run") });
  assert.equal(r.status, "skip");
  assert.match(r.detail, /TSUBAME_FASTER_WHISPER_PYTHON/);
});

test("asr CER is corpus CER over clips, transcribed in ja with the configured model", () => {
  const dir = gold([clip("a", "おやすみ。", "晚安"), clip("b", "ありがとう", "谢谢")]);
  const calls = [];
  const hyp = { a: "おやすみ", b: "ありがと" };
  const r = asrCerCheck({
    goldDir: dir,
    worker: "w.py",
    env: { ...ENV, YUZUKI_EVAL_ASR_MODEL: "small" },
    transcribe: (path, opts) => {
      calls.push(opts);
      return hyp[basename(path, ".wav")];
    },
  });
  // distances 0 + 1 over 4 + 5 reference chars
  assert.equal(r.status, "pass");
  assert.equal(r.metrics.cer, +(1 / 9).toFixed(4));
  assert.equal(r.metrics.clips, 2);
  assert.deepEqual(calls.map((c) => [c.language, c.model, c.python, c.worker]), [
    ["ja", "small", "python", "w.py"],
    ["ja", "small", "python", "w.py"],
  ]);
  assert.match(r.detail, /no gate/);
});

test("a gate above the measured CER fails", () => {
  const dir = gold([clip("a", "ありがとう", "谢谢")]);
  const env = { ...ENV, YUZUKI_EVAL_ASR_CER_MAX: "0.1" };
  const r = asrCerCheck({ goldDir: dir, worker: "w.py", env, transcribe: () => "ありが" });
  assert.equal(r.status, "fail");
  assert.equal(r.metrics.max, 0.1);
});

test("a failing runtime fails the check instead of dropping the clip", () => {
  const dir = gold([clip("a", "おやすみ", "晚安"), clip("b", "ありがとう", "谢谢")]);
  const r = asrCerCheck({
    goldDir: dir,
    worker: "w.py",
    env: ENV,
    transcribe: (path) => {
      if (path.endsWith("b.wav")) throw new Error("ASR worker failed: boom");
      return "おやすみ";
    },
  });
  assert.equal(r.status, "fail");
  assert.match(r.detail, /b: ASR worker failed: boom/);
});

test("manifest problems fail with every reason", () => {
  const bad = { ...clip("a", "おやすみ", "晚安"), rights: { ...RIGHTS, evaluation_permitted: false } };
  const dir = gold([bad]);
  writeFileSync(join(dir, "manifest.json"), JSON.stringify({ schema_version: 1, clips: [bad, { ...bad, audio: "audio/missing.wav" }] }));
  const r = asrCerCheck({ goldDir: dir, worker: "w.py", env: ENV, transcribe: () => assert.fail("must not run") });
  assert.equal(r.status, "fail");
  assert.match(r.detail, /evaluation_permitted/);
  assert.match(r.detail, /duplicate id/);
  assert.match(r.detail, /audio file not found: audio\/missing\.wav/);
});

test("tts round trip synthesizes the translation and transcribes it in that language", () => {
  const dir = gold([clip("a", "おやすみ", "晚安，明天见。")]);
  const outDir = join(dir, "out");
  const seen = {};
  const r = ttsRoundtripCheck({
    goldDir: dir,
    worker: "w.py",
    outDir,
    env: { ...ENV, YUZUKI_EVAL_TTS_CMD: "tts" },
    synthesize: (cmd, req) => {
      seen.synth = { cmd, ...req };
      writeFileSync(req.outPath, "RIFF");
    },
    transcribe: (path, opts) => {
      seen.asr = { path, language: opts.language };
      return "晚安明天见";
    },
  });
  assert.equal(r.status, "pass");
  assert.equal(r.metrics.cer, 0);
  assert.equal(seen.synth.cmd, "tts");
  assert.equal(seen.synth.text, "晚安，明天见。");
  assert.equal(seen.synth.language, "zh-Hans");
  assert.equal(seen.asr.path, join(outDir, "a.wav"));
  assert.equal(seen.asr.language, "zh");
  assert.ok(existsSync(join(outDir, "a.wav")));
});

test("tts round trip skips without a TTS command and fails without translation_language", () => {
  const c = clip("a", "おやすみ", "晚安");
  const noop = () => assert.fail("must not run");
  const skip = ttsRoundtripCheck({ goldDir: gold([c]), worker: "w.py", outDir: tmpdir(), env: ENV, synthesize: noop, transcribe: noop });
  assert.equal(skip.status, "skip");
  assert.match(skip.detail, /YUZUKI_EVAL_TTS_CMD/);
  const env = { ...ENV, YUZUKI_EVAL_TTS_CMD: "tts" };
  const fail = ttsRoundtripCheck({ goldDir: gold([c], {}), worker: "w.py", outDir: tmpdir(), env, synthesize: noop, transcribe: noop });
  assert.equal(fail.status, "fail");
  assert.match(fail.detail, /translation_language/);
});

test("asrLanguage maps BCP 47 tags to whisper codes", () => {
  assert.equal(asrLanguage("zh-Hans"), "zh");
  assert.equal(asrLanguage("EN"), "en");
  assert.equal(asrLanguage("ja"), "ja");
});
