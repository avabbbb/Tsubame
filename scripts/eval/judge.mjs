// LLM translation judge (T11). Scores candidate translations with the rubric in
// eval/rubrics/translate.md through an OpenAI-compatible /chat/completions
// endpoint (the same request shape as the Desktop's refine::chat_completion:
// one system and one user message, temperature 0). Paid APIs and local servers
// (LM Studio, Ollama, llama.cpp) take the same path.
//
// Input JSONL, one sentence per line. fixtures/gold/translate-labels.jsonl works
// as is; its human score is never sent to the judge:
//   {"id":"clip01#2","source":"...","candidate":"...","prev":"...","next":"...","reference":"..."}
// Output JSONL (stdout, or --out), one line per scored row, ready for kappa.mjs:
//   {"id","rubric_version","score","criteria","reason","model"}
// A row the judge answered with invalid output is reported on stderr and left
// out of the output, never guessed; the exit code is then 1.
//
// Configuration is explicit; nothing picks a provider for you:
//   YUZUKI_JUDGE_BASE_URL  the API base ending in /v1 (a paid API, or a local
//                          LM Studio / Ollama / llama.cpp server)
//   YUZUKI_JUDGE_MODEL     model id
//   YUZUKI_JUDGE_AUTH      bearer (default) or none
//   YUZUKI_JUDGE_API_KEY   required when YUZUKI_JUDGE_AUTH=bearer; never logged
//
// Usage: node scripts/eval/judge.mjs <input.jsonl> [--out judged.jsonl]
//          [--rubric eval/rubrics/translate.md] [--target-language zh-Hans]
//   exit 0 every row scored, 1 some rows failed, 2 bad configuration or input.

import { readFileSync, writeFileSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { parseJsonl } from "./kappa.mjs";

export const DEFAULT_RUBRIC = join(dirname(fileURLToPath(import.meta.url)), "..", "..", "eval", "rubrics", "translate.md");
export const CRITERIA = ["accuracy", "completeness", "fluency", "register", "terminology"];

const isGrade = (v) => Number.isInteger(v) && v >= 1 && v <= 5;

export function rubricVersion(rubric) {
  const m = /^rubric_version:\s*(\S+)\s*$/m.exec(rubric);
  if (!m) throw new Error("rubric has no rubric_version line");
  return m[1];
}

export function configFromEnv(env) {
  const missing = ["YUZUKI_JUDGE_BASE_URL", "YUZUKI_JUDGE_MODEL"].filter((k) => !env[k]);
  if (missing.length) throw new Error("missing " + missing.join(", "));
  const auth = env.YUZUKI_JUDGE_AUTH ?? "bearer";
  if (auth !== "bearer" && auth !== "none") {
    throw new Error("YUZUKI_JUDGE_AUTH must be bearer or none, got " + JSON.stringify(auth));
  }
  if (auth === "bearer" && !env.YUZUKI_JUDGE_API_KEY) {
    throw new Error("YUZUKI_JUDGE_API_KEY is required when YUZUKI_JUDGE_AUTH=bearer (a local server without auth: YUZUKI_JUDGE_AUTH=none)");
  }
  return { baseUrl: env.YUZUKI_JUDGE_BASE_URL, model: env.YUZUKI_JUDGE_MODEL, auth, apiKey: env.YUZUKI_JUDGE_API_KEY };
}

/** Checks one input row; returns an error message or null. */
export function rowError(row) {
  if (typeof row.id !== "string" || !row.id) return "id must be a non-empty string";
  if (typeof row.source !== "string") return "source must be a string";
  if (typeof row.candidate !== "string") return "candidate must be a string (empty for a missing translation)";
  for (const k of ["prev", "next", "reference"]) {
    if (row[k] != null && typeof row[k] !== "string") return k + " must be a string when present";
  }
  return null;
}

/** System and user messages for one row. Only these fields reach the judge. */
export function buildMessages(rubric, row, targetLanguage) {
  const system = [
    "You are a strict translation quality judge for Japanese ASMR subtitles.",
    "Apply the rubric below exactly. Score only the CURRENT sentence; the other sentences are context.",
    "Reply with one JSON object and nothing else, in the format under Judge output.",
    "",
    rubric,
  ].join("\n");
  const lines = [];
  if (targetLanguage) lines.push("Target language: " + targetLanguage);
  if (row.prev) lines.push("Previous source sentence: " + row.prev);
  lines.push("CURRENT source sentence: " + row.source);
  if (row.next) lines.push("Next source sentence: " + row.next);
  if (row.reference) lines.push("Human reference translation (a hint, not the only correct answer): " + row.reference);
  lines.push("Candidate translation to score: " + row.candidate);
  return [
    { role: "system", content: system },
    { role: "user", content: lines.join("\n") },
  ];
}

/** Parses the judge's reply. Throws on anything outside the rubric's format. */
export function parseJudgeContent(content) {
  const start = content.indexOf("{");
  const end = content.lastIndexOf("}");
  if (start < 0 || end < start) throw new Error("judge reply has no JSON object");
  let value;
  try {
    value = JSON.parse(content.slice(start, end + 1));
  } catch (e) {
    throw new Error("judge reply is not valid JSON: " + e.message);
  }
  const { score } = value;
  if (score !== "NA" && !isGrade(score)) {
    throw new Error("judge score must be an integer 1-5 or \"NA\", got " + JSON.stringify(score));
  }
  const out = { score };
  if (value.criteria != null) {
    if (typeof value.criteria !== "object" || Array.isArray(value.criteria)) {
      throw new Error("judge criteria must be an object");
    }
    const criteria = {};
    for (const k of CRITERIA) {
      const v = value.criteria[k];
      if (v == null) continue;
      if (!isGrade(v)) throw new Error("judge criteria." + k + " must be an integer 1-5, got " + JSON.stringify(v));
      criteria[k] = v;
    }
    out.criteria = criteria;
  } else if (score !== "NA") {
    throw new Error("judge criteria are required for a numeric score");
  }
  out.reason = typeof value.reason === "string" ? value.reason : "";
  return out;
}

/** One /chat/completions call; returns the message content. */
export async function chat(cfg, messages, fetchImpl = fetch) {
  const headers = { "content-type": "application/json" };
  if (cfg.auth === "bearer") headers.authorization = "Bearer " + cfg.apiKey;
  const res = await fetchImpl(cfg.baseUrl.replace(/\/+$/, "") + "/chat/completions", {
    method: "POST",
    headers,
    body: JSON.stringify({ model: cfg.model, temperature: 0, messages }),
  });
  const text = await res.text();
  if (!res.ok) throw new Error("judge provider returned HTTP " + res.status);
  let value;
  try {
    value = JSON.parse(text);
  } catch {
    throw new Error("judge provider returned invalid JSON");
  }
  const content = value?.choices?.[0]?.message?.content;
  if (typeof content !== "string") throw new Error("judge provider response has no message content");
  return content;
}

/** Judges every row in order. Failures are returned, never scored. */
export async function judgeAll(rows, { rubric, cfg, targetLanguage, fetchImpl = fetch }) {
  const version = rubricVersion(rubric);
  const judged = [];
  const failures = [];
  for (const row of rows) {
    const bad = rowError(row);
    if (bad) {
      failures.push({ id: row.id ?? null, error: bad });
      continue;
    }
    try {
      const content = await chat(cfg, buildMessages(rubric, row, targetLanguage), fetchImpl);
      judged.push({ id: row.id, rubric_version: version, ...parseJudgeContent(content), model: cfg.model });
    } catch (e) {
      failures.push({ id: row.id, error: e.message });
    }
  }
  return { judged, failures };
}

async function main(argv, env) {
  const positional = [];
  const opts = { rubric: DEFAULT_RUBRIC, out: null, targetLanguage: null };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--out") opts.out = argv[++i];
    else if (argv[i] === "--rubric") opts.rubric = argv[++i];
    else if (argv[i] === "--target-language") opts.targetLanguage = argv[++i];
    else positional.push(argv[i]);
  }
  if (positional.length !== 1) {
    console.error("usage: node scripts/eval/judge.mjs <input.jsonl> [--out judged.jsonl] [--rubric path] [--target-language tag]");
    return 2;
  }
  const cfg = configFromEnv(env);
  const rubric = readFileSync(opts.rubric, "utf8");
  const rows = parseJsonl(readFileSync(positional[0], "utf8"), positional[0]);
  const { judged, failures } = await judgeAll(rows, { rubric, cfg, targetLanguage: opts.targetLanguage });
  const body = judged.map((r) => JSON.stringify(r) + "\n").join("");
  if (opts.out) writeFileSync(opts.out, body);
  else process.stdout.write(body);
  for (const f of failures) console.error("failed " + f.id + ": " + f.error);
  console.error("judged " + judged.length + " of " + rows.length + " rows, " + failures.length + " failed");
  return failures.length ? 1 : 0;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  main(process.argv.slice(2), process.env).then(
    (code) => {
      process.exitCode = code;
    },
    (e) => {
      console.error(e.message);
      process.exitCode = 2;
    },
  );
}
