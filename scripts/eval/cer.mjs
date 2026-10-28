// Character Error Rate (CER) for Japanese transcripts.
//
//   CER = Levenshtein(normalize(ref), normalize(hyp)) / length(normalize(ref))
//
// Distance and length are counted in Unicode code points (so 𠮷 is one char).
// CER can exceed 1.0 when the hypothesis has many insertions.
//
// Japanese normalization (normalizeJa), applied to both sides:
//   1. Unicode NFKC: full-width ASCII -> ASCII, half-width kana -> full-width,
//      U+3000 ideographic space -> U+0020, compatibility forms folded.
//   2. Remove every whitespace code point (\s).
//   3. Remove every punctuation (\p{P}) and symbol (\p{S}) code point,
//      e.g. 、。「」・！？… ♪ ~. The prolonged sound mark ー (U+30FC) is a letter
//      (Lm) and is KEPT.
//   4. Kana folding is OFF by default: ありがとう vs アリガトウ is a real
//      difference. With { foldKana: true }, katakana U+30A1..U+30F6 is mapped
//      to hiragana (code point - 0x60) before comparison. Kanji vs kana
//      readings are never folded.
// Case is not folded (NFKC keeps ASCII case).

const KATAKANA_FIRST = 0x30a1;
const KATAKANA_LAST = 0x30f6;

export function normalizeJa(text, { foldKana = false } = {}) {
  let s = String(text ?? "").normalize("NFKC").replace(/[\s\p{P}\p{S}]/gu, "");
  if (foldKana) {
    s = Array.from(s, (ch) => {
      const cp = ch.codePointAt(0);
      return cp >= KATAKANA_FIRST && cp <= KATAKANA_LAST ? String.fromCodePoint(cp - 0x60) : ch;
    }).join("");
  }
  return s;
}

/** Levenshtein distance over code points (unit cost insert/delete/substitute). */
export function levenshtein(a, b) {
  const x = Array.from(a);
  const y = Array.from(b);
  if (x.length < y.length) return levenshtein(b, a);
  let prev = Array.from({ length: y.length + 1 }, (_, j) => j);
  let cur = new Array(y.length + 1);
  for (let i = 1; i <= x.length; i++) {
    cur[0] = i;
    for (let j = 1; j <= y.length; j++) {
      const sub = prev[j - 1] + (x[i - 1] === y[j - 1] ? 0 : 1);
      cur[j] = Math.min(sub, prev[j] + 1, cur[j - 1] + 1);
    }
    [prev, cur] = [cur, prev];
  }
  return prev[y.length];
}

/** Distance and reference length after normalization; throws on an empty reference. */
export function cerParts(reference, hypothesis, options = {}) {
  const ref = normalizeJa(reference, options);
  const hyp = normalizeJa(hypothesis, options);
  const refLen = Array.from(ref).length;
  if (refLen === 0) throw new RangeError("CER undefined: reference is empty after normalization");
  return { distance: levenshtein(ref, hyp), refLen };
}

export function cer(reference, hypothesis, options = {}) {
  const { distance, refLen } = cerParts(reference, hypothesis, options);
  return distance / refLen;
}

/** Corpus CER: sum of distances / sum of reference lengths (not a mean of per-clip CERs). */
export function corpusCer(pairs, options = {}) {
  if (!pairs.length) throw new RangeError("CER undefined: no pairs");
  let distance = 0;
  let refLen = 0;
  for (const [reference, hypothesis] of pairs) {
    const p = cerParts(reference, hypothesis, options);
    distance += p.distance;
    refLen += p.refLen;
  }
  return distance / refLen;
}
