// Judge parsing, prompt and transport, with a fake fetch. Run: node --test scripts/eval/
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import {
  DEFAULT_RUBRIC,
  buildMessages,
  configFromEnv,
  judgeAll,
  parseJudgeContent,
  rubricVersion,
} from "./judge.mjs";

const RUBRIC = "# Rubric\n\nrubric_version: v9\n\nScore 1-5.\n";
const BASE = ["http:", "", "judge.test", "v1"].join("/"); // no URL literal in source
const CFG = { baseUrl: BASE + "/", model: "m1", auth: "bearer", apiKey: "sk-test-secret" };
const FENCE = "\u0060\u0060\u0060"; // a Markdown code fence
const GOOD = "{\"score\":4,\"criteria\":{\"accuracy\":5,\"completeness\":5,\"fluency\":4,\"register\":4,\"terminology\":5},\"reason\":\"ok\"}";

function fakeFetch(replies) {
  const calls = [];
  const fn = async (url, init) => {
    calls.push({ url, init, body: JSON.parse(init.body) });
    const r = replies.shift();
    if (r.status && r.status !== 200) return { ok: false, status: r.status, text: async () => "nope" };
    return { ok: true, status: 200, text: async () => JSON.stringify({ choices: [{ message: { content: r.content } }] }) };
  };
  fn.calls = calls;
  return fn;
}

test("the shipped rubric declares v1", () => {
  assert.equal(rubricVersion(readFileSync(DEFAULT_RUBRIC, "utf8")), "v1");
  assert.throws(() => rubricVersion("no version"), /rubric_version/);
});

test("parses fenced JSON and NA", () => {
  const r = parseJudgeContent("Sure:\n" + FENCE + "json\n" + GOOD + "\n" + FENCE);
  assert.equal(r.score, 4);
  assert.equal(r.criteria.register, 4);
  assert.deepEqual(parseJudgeContent("{\"score\":\"NA\",\"reason\":\"music only\"}"), { score: "NA", reason: "music only" });
});

test("rejects scores and criteria outside the rubric", () => {
  assert.throws(() => parseJudgeContent("looks fine"), /no JSON/);
  assert.throws(() => parseJudgeContent("{\"score\":\"4\",\"criteria\":{}}"), /1-5/);
  assert.throws(() => parseJudgeContent("{\"score\":6,\"criteria\":{}}"), /1-5/);
  assert.throws(() => parseJudgeContent("{\"score\":3.5,\"criteria\":{}}"), /1-5/);
  assert.throws(() => parseJudgeContent("{\"score\":3}"), /criteria are required/);
  assert.throws(() => parseJudgeContent("{\"score\":3,\"criteria\":{\"accuracy\":0}}"), /accuracy/);
});

test("the human score never reaches the judge", () => {
  const row = { id: "a", score: "HUMAN-SCORE-MARKER", source: "おやすみ。", candidate: "晚安。", prev: "p", next: "n", reference: "晚安。" };
  const [system, user] = buildMessages(RUBRIC, row, "zh-Hans");
  assert.ok(system.content.includes("rubric_version: v9"));
  assert.ok(user.content.includes("CURRENT source sentence: おやすみ。"));
  assert.ok(user.content.includes("Target language: zh-Hans"));
  assert.ok(!JSON.stringify([system, user]).includes("HUMAN-SCORE-MARKER"));
});

test("configuration is explicit", () => {
  assert.throws(() => configFromEnv({}), /YUZUKI_JUDGE_BASE_URL, YUZUKI_JUDGE_MODEL/);
  const base = { YUZUKI_JUDGE_BASE_URL: BASE, YUZUKI_JUDGE_MODEL: "m" };
  assert.throws(() => configFromEnv(base), /YUZUKI_JUDGE_API_KEY/);
  assert.throws(() => configFromEnv({ ...base, YUZUKI_JUDGE_AUTH: "basic" }), /bearer or none/);
  assert.equal(configFromEnv({ ...base, YUZUKI_JUDGE_AUTH: "none" }).auth, "none");
});

test("judgeAll scores good rows and reports the rest without guessing", async () => {
  const fetchImpl = fakeFetch([{ content: GOOD }, { content: "I think 4" }, { status: 500 }]);
  const rows = [
    { id: "a", source: "s1", candidate: "c1", score: 2 },
    { id: "b", source: "s2", candidate: "c2" },
    { id: "c", source: "s3", candidate: "c3" },
    { id: "d", source: "s4" },
  ];
  const { judged, failures } = await judgeAll(rows, { rubric: RUBRIC, cfg: CFG, fetchImpl });
  assert.deepEqual(judged.map((r) => [r.id, r.rubric_version, r.score, r.model]), [["a", "v9", 4, "m1"]]);
  assert.deepEqual(failures.map((f) => f.id), ["b", "c", "d"]);
  assert.match(failures[1].error, /HTTP 500/);
  assert.ok(!JSON.stringify(failures).includes("sk-test-secret"));
  const call = fetchImpl.calls[0];
  assert.equal(call.url, BASE + "/chat/completions");
  assert.equal(call.init.headers.authorization, "Bearer sk-test-secret");
  assert.equal(call.body.temperature, 0);
  assert.equal(call.body.model, "m1");
  assert.equal(fetchImpl.calls.length, 3);
});
