// Agreement between the LLM translation judge and human raters (T11).
//
// Inputs, both JSONL, joined on id:
//   labels  fixtures/gold/translate-labels.jsonl, one human-scored sentence per line:
//           {"id":"clip01#2","rubric_version":"v1","score":4,"source":"...","candidate":"..."}
//           score is an integer 1-5 or "NA" (eval/rubrics/translate.md). Optional
//           fields scripts/eval/judge.mjs reads: prev, next, reference.
//   judged  output of scripts/eval/judge.mjs: {"id","rubric_version","score",...}
//
// Only rows with the same rubric_version on both sides are compared.
// Headline metric: quadratic-weighted Cohen's kappa over rows where both sides
// gave a number (scores are ordinal). Unweighted kappa and NA agreement are
// reported beside it; a row NA on one side only counts in na_one, not in kappa.
// Acceptance (T11): kappa_quadratic >= 0.6 on 100-200 compared rows.
//
// Usage: node scripts/eval/kappa.mjs <labels.jsonl> <judged.jsonl> [--min 0.6] [--min-rows 100]
//   exit 0 when kappa_quadratic >= min, 1 below it, 2 on bad input or too few rows.

import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { fileURLToPath } from "node:url";

export const SCORES = [1, 2, 3, 4, 5];

/**
 * Cohen's kappa for paired ratings. weights: "none" (unweighted) or "quadratic".
 * Returns null when it is undefined: no pairs, or chance disagreement is 0
 * (both raters used a single category).
 */
export function cohenKappa(pairs, { categories = SCORES, weights = "none" } = {}) {
  const k = categories.length;
  const n = pairs.length;
  if (n === 0) return null;
  const index = new Map(categories.map((c, i) => [c, i]));
  const observed = Array.from({ length: k }, () => new Array(k).fill(0));
  for (const [a, b] of pairs) {
    const i = index.get(a);
    const j = index.get(b);
    if (i === undefined || j === undefined) {
      throw new RangeError("score outside " + categories.join(",") + ": " + a + ", " + b);
    }
    observed[i][j] += 1;
  }
  const rows = observed.map((r) => r.reduce((s, v) => s + v, 0));
  const cols = categories.map((_, j) => observed.reduce((s, r) => s + r[j], 0));
  const w = (i, j) => (weights === "quadratic" ? ((i - j) / (k - 1)) ** 2 : i === j ? 0 : 1);
  let disagreeObserved = 0;
  let disagreeExpected = 0;
  for (let i = 0; i < k; i++) {
    for (let j = 0; j < k; j++) {
      disagreeObserved += (w(i, j) * observed[i][j]) / n;
      disagreeExpected += w(i, j) * (rows[i] / n) * (cols[j] / n);
    }
  }
  return disagreeExpected === 0 ? null : 1 - disagreeObserved / disagreeExpected;
}

export function parseJsonl(text, label = "jsonl") {
  return text
    .split(/\r?\n/)
    .map((line, i) => [line.trim(), i + 1])
    .filter(([line]) => line)
    .map(([line, no]) => {
      try {
        return JSON.parse(line);
      } catch (e) {
        throw new SyntaxError(label + ":" + no + ": " + e.message);
      }
    });
}

const isScore = (s) => s === "NA" || SCORES.includes(s);

/** Joins human labels and judge output on id and scores their agreement. */
export function agreement(labels, judged) {
  const byId = new Map(judged.map((r) => [r.id, r]));
  const pairs = [];
  let missing = 0;
  let versionMismatch = 0;
  let naBoth = 0;
  let naOne = 0;
  for (const h of labels) {
    const j = byId.get(h.id);
    if (!j) {
      missing += 1;
      continue;
    }
    if (h.rubric_version !== j.rubric_version) {
      versionMismatch += 1;
      continue;
    }
    if (!isScore(h.score) || !isScore(j.score)) {
      throw new RangeError(h.id + ': score must be 1-5 or "NA" (human ' + h.score + ", judge " + j.score + ")");
    }
    const hNa = h.score === "NA";
    const jNa = j.score === "NA";
    if (hNa && jNa) naBoth += 1;
    else if (hNa || jNa) naOne += 1;
    else pairs.push([h.score, j.score]);
  }
  return {
    compared: pairs.length,
    missing,
    version_mismatch: versionMismatch,
    na_both: naBoth,
    na_one: naOne,
    kappa_quadratic: cohenKappa(pairs, { weights: "quadratic" }),
    kappa: cohenKappa(pairs),
  };
}

function main(argv) {
  const positional = [];
  const opts = { min: 0.6, minRows: 100 };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--min") opts.min = Number(argv[++i]);
    else if (argv[i] === "--min-rows") opts.minRows = Number(argv[++i]);
    else positional.push(argv[i]);
  }
  if (positional.length !== 2) {
    console.error("usage: node scripts/eval/kappa.mjs <labels.jsonl> <judged.jsonl> [--min 0.6] [--min-rows 100]");
    return 2;
  }
  const [labelsPath, judgedPath] = positional;
  const report = agreement(
    parseJsonl(readFileSync(labelsPath, "utf8"), labelsPath),
    parseJsonl(readFileSync(judgedPath, "utf8"), judgedPath),
  );
  console.log(JSON.stringify(report, null, 2));
  if (report.compared < opts.minRows) {
    console.error("too few comparable rows: " + report.compared + " < " + opts.minRows);
    return 2;
  }
  if (report.kappa_quadratic == null) {
    console.error("kappa undefined: judge and humans each used a single score");
    return 2;
  }
  return report.kappa_quadratic >= opts.min ? 0 : 1;
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    process.exitCode = main(process.argv.slice(2));
  } catch (e) {
    console.error(e.message);
    process.exitCode = 2;
  }
}
