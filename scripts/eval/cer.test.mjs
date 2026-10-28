// Hand-computed CER cases. Run: node --test scripts/eval/
import { test } from "node:test";
import assert from "node:assert/strict";
import { cer, corpusCer, levenshtein, normalizeJa } from "./cer.mjs";

test("levenshtein classic case", () => {
  assert.equal(levenshtein("kitten", "sitting"), 3); // k->s, e->i, +g
  assert.equal(levenshtein("", "あいう"), 3);
  assert.equal(levenshtein("あいう", "あいう"), 0);
});

test("identical transcript is 0", () => {
  assert.equal(cer("今日は来てくれてありがとう", "今日は来てくれてありがとう"), 0);
});

test("one substitution over 5 chars", () => {
  assert.equal(cer("あいうえお", "あいうえか"), 1 / 5);
});

test("one deletion over 5 chars", () => {
  assert.equal(cer("ありがとう", "ありがと"), 1 / 5);
});

test("one insertion over 4 chars", () => {
  assert.equal(cer("おやすみ", "おやすみね"), 1 / 4);
});

test("insertions can push CER above 1", () => {
  assert.equal(cer("あ", "いうえ"), 3); // 1 substitution + 2 insertions over 1 char
});

test("punctuation and whitespace are ignored", () => {
  assert.equal(cer("今日は、来てくれて　ありがとう。", "今日は来てくれてありがとう"), 0);
  assert.equal(cer("「おやすみ」！", "おやすみ"), 0);
});

test("NFKC folds full-width and half-width forms", () => {
  assert.equal(normalizeJa("ＡＢＣ１２３"), "ABC123");
  assert.equal(normalizeJa("ｱﾘｶﾞﾄｳ"), "アリガトウ");
  assert.equal(cer("ＡＢＣ", "ABC"), 0);
});

test("prolonged sound mark is kept", () => {
  assert.equal(normalizeJa("ラーメン。"), "ラーメン");
  assert.equal(cer("ラーメン", "ラメン"), 1 / 4);
});

test("kana is not folded by default, folded on request", () => {
  assert.equal(cer("ありがとう", "アリガトウ"), 1);
  assert.equal(cer("ありがとう", "アリガトウ", { foldKana: true }), 0);
});

test("astral characters count as one char", () => {
  assert.equal(cer("𠮷野家", "吉野家"), 1 / 3);
});

test("corpus CER pools distances and lengths", () => {
  const pairs = [
    ["あいうえお", "あいうえか"], // 1 / 5
    ["おやすみ", "おやすみね"], // 1 / 4
  ];
  assert.equal(corpusCer(pairs), 2 / 9);
});

test("empty reference is rejected, never scored", () => {
  assert.throws(() => cer("。", "あ"), RangeError);
  assert.throws(() => corpusCer([]), RangeError);
});
