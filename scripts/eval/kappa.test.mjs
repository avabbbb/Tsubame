// Hand-computed agreement cases. Run: node --test scripts/eval/
import { test } from "node:test";
import assert from "node:assert/strict";
import { agreement, cohenKappa, parseJsonl } from "./kappa.mjs";

const repeat = (pair, n) => Array.from({ length: n }, () => pair);
const close = (a, b) => assert.ok(Math.abs(a - b) < 1e-12, a + " != " + b);

test("unweighted kappa, classic 2x2 table", () => {
  // po = 35/50 = 0.7, pe = 0.5*0.6 + 0.5*0.4 = 0.5, kappa = 0.4
  const pairs = [...repeat([1, 1], 20), ...repeat([1, 2], 5), ...repeat([2, 1], 10), ...repeat([2, 2], 15)];
  close(cohenKappa(pairs, { categories: [1, 2] }), 0.4);
});

test("quadratic weights penalise distance", () => {
  // observed weighted disagreement 1/4, expected 13/32 -> 1 - 8/13 = 5/13
  const pairs = [[1, 1], [2, 2], [3, 3], [1, 3]];
  close(cohenKappa(pairs, { categories: [1, 2, 3], weights: "quadratic" }), 5 / 13);
});

test("perfect agreement is 1, undefined cases are null", () => {
  assert.equal(cohenKappa([[1, 1], [5, 5], [3, 3]]), 1);
  assert.equal(cohenKappa([]), null);
  assert.equal(cohenKappa([[4, 4], [4, 4]]), null);
});

test("out-of-range score is rejected", () => {
  assert.throws(() => cohenKappa([[0, 1]]), RangeError);
});

test("agreement joins on id, keeps NA and versions apart", () => {
  const labels = [
    { id: "a", rubric_version: "v1", score: 5 },
    { id: "b", rubric_version: "v1", score: 2 },
    { id: "c", rubric_version: "v1", score: "NA" },
    { id: "d", rubric_version: "v1", score: "NA" },
    { id: "e", rubric_version: "v0", score: 3 },
    { id: "f", rubric_version: "v1", score: 4 },
  ];
  const judged = [
    { id: "a", rubric_version: "v1", score: 5 },
    { id: "b", rubric_version: "v1", score: 2 },
    { id: "c", rubric_version: "v1", score: "NA" },
    { id: "d", rubric_version: "v1", score: 1 },
    { id: "e", rubric_version: "v1", score: 3 },
  ];
  const r = agreement(labels, judged);
  assert.deepEqual(
    { ...r, kappa: undefined, kappa_quadratic: undefined },
    { compared: 2, missing: 1, version_mismatch: 1, na_both: 1, na_one: 1, kappa: undefined, kappa_quadratic: undefined },
  );
  assert.equal(r.kappa, 1);
  assert.equal(r.kappa_quadratic, 1);
});

test("bad JSONL names the line", () => {
  assert.throws(() => parseJsonl('{"id":1}\n{oops', "labels"), /labels:2:/);
});
