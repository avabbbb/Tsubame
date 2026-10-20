#!/usr/bin/env node
// YUZUKI eval harness: automated checks + audit scorecard -> eval/results/latest.json + eval/report/index.html
// Usage: node scripts/eval/run.mjs [--fast]   (--fast skips frontend build and Rust; CI eval job has no Tauri deps)
import { spawnSync } from "node:child_process";
import { existsSync, mkdirSync, readFileSync, readdirSync, writeFileSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), "..", "..");
const FAST = process.argv.includes("--fast");
const PY = process.platform === "win32" ? "python" : "python3";
const results = [];

function sh(cmd, cwd = ROOT, timeout = 20 * 60_000) {
  const t0 = Date.now();
  const r = spawnSync(cmd, { cwd, shell: true, encoding: "utf8", timeout, maxBuffer: 64 << 20 });
  const out = `${r.stdout ?? ""}${r.stderr ?? ""}`;
  return { code: r.status ?? 1, out, ms: Date.now() - t0 };
}
const tail = (s, n = 20) => s.trim().split(/\r?\n/).slice(-n).join("\n");

function record(id, dimension, title, status, detail, extra = {}) {
  results.push({ id, dimension, title, status, detail, ...extra });
  const mark = { pass: "PASS", warn: "WARN", fail: "FAIL", skip: "SKIP" }[status];
  console.log(`[${mark}] ${dimension} / ${title}${detail ? " - " + detail.split("\n")[0] : ""}`);
}

const failureLines = (out) => {
  const hits = out.split(/\r?\n/).filter((l) => /FAILED|panicked|error(\[|:)|STATUS_|overflow/.test(l));
  return hits.length ? hits.slice(0, 15).join("\n") : tail(out);
};

function cmdCheck(id, dimension, title, cmd, { cwd, parse, retry = false } = {}) {
  let r = sh(cmd, cwd);
  let flaky = null;
  if (r.code !== 0 && retry) {
    flaky = failureLines(r.out);
    r = sh(cmd, cwd);
  }
  const metrics = parse ? parse(r.out) : {};
  const status = r.code !== 0 ? "fail" : flaky ? "warn" : "pass";
  const detail =
    r.code !== 0 ? failureLines(r.out) : flaky ? `flaky: passed on retry; first run:\n${flaky}` : metrics.summary ?? "";
  record(id, dimension, title, status, detail, { ms: r.ms, metrics: flaky ? { ...metrics, flaky } : metrics });
}

function walk(dir, exts, acc = []) {
  if (!existsSync(dir)) return acc;
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    if (e.name === "node_modules" || e.name === "target" || e.name.startsWith(".")) continue;
    const p = join(dir, e.name);
    if (e.isDirectory()) walk(p, exts, acc);
    else if (exts.some((x) => e.name.endsWith(x))) acc.push(p);
  }
  return acc;
}
const read = (p) => readFileSync(p, "utf8");

// ---------- Documentation ----------
cmdCheck("docs", "Documentation", "Docs contract (scripts/check_docs.py)", `${PY} scripts/check_docs.py`);

// ---------- Correctness: worker tests (pytest when present, else unittest) ----------
{
  const hasPytest = sh(`${PY} -c "import pytest"`).code === 0;
  const parse = (out) => {
    const ran = /Ran (\d+) tests?/.exec(out) ?? /(\d+) passed/.exec(out);
    return { tests: ran ? Number(ran[1]) : null, summary: ran ? `${ran[1]} tests` : "" };
  };
  const w = join(ROOT, "workers", "dubber");
  if (hasPytest) {
    cmdCheck("workers", "Correctness", "Worker tests (pytest)", `${PY} -m pytest spatial/tests tests -q`, { cwd: w, parse });
  } else {
    cmdCheck("workers-spatial", "Correctness", "Spatial worker tests (unittest)", `${PY} -m unittest discover -s spatial/tests -t .`, { cwd: w, parse });
    cmdCheck("workers-protocol", "Correctness", "Dubber protocol tests (unittest)", `${PY} -m unittest discover -s tests`, { cwd: w, parse });
  }
}

// ---------- Full-only: frontend + Rust ----------
if (FAST) {
  for (const [id, dim, title] of [
    ["frontend-build", "Maintainability", "Frontend typecheck + build"],
    ["cargo-fmt", "Maintainability", "rustfmt"],
    ["cargo-test", "Correctness", "Rust test suite"],
  ])
    record(id, dim, title, "skip", "skipped in --fast");
} else {
  const app = join(ROOT, "app");
  if (!existsSync(join(app, "node_modules"))) sh("npm ci", app);
  cmdCheck("frontend-build", "Maintainability", "Frontend typecheck + build", "npm run build", {
    cwd: app,
    parse: (out) => {
      const m = /index-[\w-]+\.js\s+([\d.]+) kB.*?gzip:\s+([\d.]+) kB/.exec(out);
      return m ? { js_kb: Number(m[1]), js_gzip_kb: Number(m[2]), summary: `JS ${m[1]} kB (${m[2]} kB gzip)` } : {};
    },
  });
  cmdCheck("cargo-fmt", "Maintainability", "rustfmt", "cargo fmt --all -- --check");
  cmdCheck("cargo-test", "Correctness", "Rust test suite", "cargo test --workspace --no-fail-fast", {
    retry: true,
    parse: (out) => {
      let passed = 0, failed = 0;
      for (const m of out.matchAll(/test result: \w+\. (\d+) passed; (\d+) failed/g)) {
        passed += Number(m[1]);
        failed += Number(m[2]);
      }
      return { passed, failed, summary: `${passed} passed, ${failed} failed` };
    },
  });
}

// ---------- First-run UX (static) ----------
{
  const src = join(ROOT, "app", "src");
  const files = walk(src, [".tsx", ".ts"]);
  const bodies = new Map(files.map((f) => [f, read(f)]));
  const orphans = files
    .filter((f) => f.endsWith(".tsx") && !/(main|App)\.tsx$/.test(f) && /export default/.test(bodies.get(f)))
    .filter((f) => {
      const name = f.split(/[\\/]/).pop().replace(/\.tsx$/, "");
      const re = new RegExp(`from\\s+["']\\./${name}["']`);
      return ![...bodies].some(([g, b]) => g !== f && re.test(b));
    })
    .map((f) => relative(ROOT, f).replaceAll("\\", "/"));
  record("ux-orphans", "First-run UX", "No finished screens left unmounted", orphans.length ? "fail" : "pass",
    orphans.length ? `unmounted: ${orphans.join(", ")}` : "all screens reachable", { metrics: { orphans } });

  const app = bodies.get(join(src, "App.tsx")) ?? "";
  const placeholders = (app.match(/is reserved|ready for its next adapter/g) ?? []).length;
  record("ux-placeholders", "First-run UX", "No placeholder surfaces in navigation", placeholders ? "warn" : "pass",
    `${placeholders} placeholder string(s) in App.tsx`, { metrics: { placeholders } });

  const all = [...bodies.values()].join("\n");
  const rawErrors = (all.match(/set\w+\(\s*(?:`[^`\n]*\$\{)?String\((?:err|error|e)\)|message:\s*String\((?:err|error|e)\)/g) ?? []).length;
  const notices = (all.match(/<ErrorNotice\b|userMessage\(/g) ?? []).length;
  const ratio = notices + rawErrors ? notices / (notices + rawErrors) : 1;
  record("ux-errors", "First-run UX", "Errors shown as friendly messages", ratio >= 0.8 ? "pass" : ratio >= 0.5 ? "warn" : "fail",
    `${notices} friendly vs ${rawErrors} raw error renders (${Math.round(ratio * 100)}%)`, { metrics: { notices, rawErrors } });

  const i18n = /i18next|react-intl|useTranslation|locales\//.test(all) || existsSync(join(src, "locales"));
  record("ux-i18n", "First-run UX", "Localization scaffold present", i18n ? "pass" : "warn",
    i18n ? "found" : "English-only strings; no locale layer");
}

// ---------- Security (static) ----------
{
  const files = walk(join(ROOT, "app"), [".ts", ".tsx", ".rs", ".json"]).concat(walk(join(ROOT, "workers"), [".py"]));
  const hits = [];
  const secret = /(sk-[A-Za-z0-9]{20,}|AKIA[0-9A-Z]{16}|-----BEGIN (?:RSA |EC )?PRIVATE KEY-----)/;
  for (const f of files) if (secret.test(read(f))) hits.push(relative(ROOT, f).replaceAll("\\", "/"));
  record("sec-secrets", "Security", "No hard-coded secrets", hits.length ? "fail" : "pass",
    hits.length ? hits.join(", ") : `${files.length} files scanned`);

  const conf = join(ROOT, "app", "src-tauri", "tauri.conf.json");
  if (existsSync(conf)) {
    const csp = JSON.parse(read(conf))?.app?.security?.csp;
    record("sec-csp", "Security", "Tauri CSP set", csp ? "pass" : "fail", csp ? "csp configured" : "app.security.csp is null");
  }
}

// ---------- Release (static) ----------
{
  const wf = join(ROOT, ".github", "workflows");
  const all = existsSync(wf) ? readdirSync(wf).map((f) => read(join(wf, f))).join("\n") : "";
  record("rel-signing", "Release", "Windows code signing in release workflow",
    /signtool|WINDOWS_CERTIFICATE|azure\/trusted-signing|certificateThumbprint/i.test(all) ? "pass" : "warn",
    "unsigned installers trigger SmartScreen on first run");
  record("rel-eval-ci", "Release", "Eval workflow has a harness", existsSync(join(ROOT, "scripts", "eval", "run.mjs")) ? "pass" : "fail", "");
}

// ---------- Scoring ----------
const POINTS = { pass: 5, warn: 3, fail: 1 };
const scorecardPath = join(ROOT, "eval", "scorecard.json");
const scorecard = existsSync(scorecardPath) ? JSON.parse(read(scorecardPath)) : { dimensions: {} };
const dims = {};
for (const r of results) {
  if (r.status === "skip") continue;
  (dims[r.dimension] ??= []).push(POINTS[r.status]);
}
const dimensions = {};
for (const name of new Set([...Object.keys(dims), ...Object.keys(scorecard.dimensions ?? {})])) {
  const auto = dims[name] ? dims[name].reduce((a, b) => a + b, 0) / dims[name].length : null;
  const audit = scorecard.dimensions?.[name]?.audit_score ?? null;
  const weight = scorecard.dimensions?.[name]?.weight ?? 1;
  const parts = [auto, audit].filter((x) => x != null);
  dimensions[name] = {
    auto: auto && +auto.toFixed(2),
    audit,
    weight,
    score: parts.length ? +(parts.reduce((a, b) => a + b, 0) / parts.length).toFixed(2) : null,
  };
}
const scored = Object.values(dimensions).filter((d) => d.score != null);
const wsum = scored.reduce((a, d) => a + d.weight, 0);
const overall = wsum ? +(scored.reduce((a, d) => a + d.score * d.weight, 0) / wsum).toFixed(2) : null;
const gate = scorecard.beta_gate ?? 3.5;
const openP0 = (scorecard.findings ?? []).filter((f) => f.priority === "P0" && f.status !== "fixed").length;
const failed = results.filter((r) => r.status === "fail").length;
const verdict = openP0 === 0 && failed === 0 && overall != null && overall >= gate ? "beta-ready" : "not-ready";

const commit = sh("git rev-parse --short HEAD").out.trim();
const report = {
  generated_at: new Date().toISOString(),
  commit,
  mode: FAST ? "fast" : "full",
  overall,
  beta_gate: gate,
  verdict,
  open_p0: openP0,
  dimensions,
  checks: results,
  findings: scorecard.findings ?? [],
};

// ---------- Persist (latest + history for deltas) ----------
const resDir = join(ROOT, "eval", "results");
mkdirSync(resDir, { recursive: true });
const histPath = join(resDir, "history.json");
const history = existsSync(histPath) ? JSON.parse(read(histPath)) : [];
history.push({ at: report.generated_at, commit, mode: report.mode, overall, dimensions: Object.fromEntries(Object.entries(dimensions).map(([k, v]) => [k, v.score])) });
writeFileSync(histPath, JSON.stringify(history.slice(-50), null, 2));
writeFileSync(join(resDir, "latest.json"), JSON.stringify(report, null, 2));

// ---------- HTML dashboard ----------
const esc = (s) => String(s ?? "").replace(/[&<>"]/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;" })[c]);
const color = (s) => (s == null ? "#666" : s >= 4 ? "#2e9d5b" : s >= 3 ? "#d9a21b" : "#d0453a");
const bars = Object.entries(dimensions)
  .map(([k, d]) => `<div class="bar"><span>${esc(k)}</span><div><i style="width:${((d.score ?? 0) / 5) * 100}%;background:${color(d.score)}"></i></div><b>${d.score ?? "–"}</b><small>auto ${d.auto ?? "–"} · audit ${d.audit ?? "–"}</small></div>`)
  .join("");
const trend = history.slice(-20);
const pts = trend.map((h, i) => `${(i / Math.max(1, trend.length - 1)) * 300},${100 - ((h.overall ?? 0) / 5) * 100}`).join(" ");
const rows = results
  .map((r) => `<tr class="${r.status}"><td>${r.status.toUpperCase()}</td><td>${esc(r.dimension)}</td><td>${esc(r.title)}</td><td><pre>${esc(r.detail)}</pre></td><td>${r.ms ? (r.ms / 1000).toFixed(1) + "s" : ""}</td></tr>`)
  .join("");
const frows = report.findings
  .map((f) => `<tr class="${f.status === "fixed" ? "pass" : f.priority === "P0" ? "fail" : "warn"}"><td>${esc(f.id)}</td><td>${esc(f.priority)}</td><td>${esc(f.dimension)}</td><td>${esc(f.title)}</td><td>${esc(f.status)}</td></tr>`)
  .join("");
const html = `<!doctype html><html lang="en"><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>YUZUKI eval ${esc(commit)}</title>
<style>
body{font:14px/1.5 system-ui,sans-serif;background:#111418;color:#e6e6e6;margin:0;padding:24px;max-width:1100px;margin:auto}
h1{margin:0}.meta{color:#999}.hero{display:flex;gap:24px;align-items:center;margin:20px 0}
.big{font-size:56px;font-weight:700}.pill{padding:4px 10px;border-radius:99px;font-weight:600}
.beta-ready{background:#1f5b37}.not-ready{background:#6b2620}
.bar{display:grid;grid-template-columns:160px 1fr 50px 140px;gap:10px;align-items:center;margin:6px 0}
.bar div{background:#252a31;height:12px;border-radius:6px;overflow:hidden}.bar i{display:block;height:100%}
.bar small{color:#999}table{width:100%;border-collapse:collapse;margin-top:12px}
td,th{border-bottom:1px solid #2a2f36;padding:6px;text-align:left;vertical-align:top}
pre{margin:0;white-space:pre-wrap;font-size:12px;max-height:120px;overflow:auto}
tr.pass td:first-child{color:#2e9d5b}tr.warn td:first-child{color:#d9a21b}tr.fail td:first-child{color:#d0453a}tr.skip td:first-child{color:#666}
svg{background:#181c21;border-radius:8px}
</style>
<h1>YUZUKI evaluation</h1>
<div class="meta">${esc(report.generated_at)} · commit ${esc(commit)} · ${report.mode} run</div>
<div class="hero"><div class="big" style="color:${color(overall)}">${overall ?? "–"}<small style="font-size:20px">/5</small></div>
<div><span class="pill ${verdict}">${verdict}</span><div class="meta">beta gate ≥ ${gate}, no open P0, no failing checks · open P0: ${openP0} · failing checks: ${failed}</div></div>
<svg width="320" height="110" viewBox="-10 -5 320 110"><polyline fill="none" stroke="#6aa9ff" stroke-width="2" points="${pts}"/></svg></div>
<h2>Dimensions</h2>${bars}
<h2>Automated checks</h2><table><tr><th></th><th>Dimension</th><th>Check</th><th>Detail</th><th>Time</th></tr>${rows}</table>
<h2>Audit findings</h2><table><tr><th>ID</th><th>Pri</th><th>Dimension</th><th>Finding</th><th>Status</th></tr>${frows}</table>
</html>`;
const repDir = join(ROOT, "eval", "report");
mkdirSync(repDir, { recursive: true });
writeFileSync(join(repDir, "index.html"), html);

console.log(`\noverall ${overall}/5 · ${verdict} · open P0 ${openP0} · failing checks ${failed}`);
console.log(`report: ${relative(ROOT, join(repDir, "index.html"))}`);
process.exit(failed ? 1 : 0);
