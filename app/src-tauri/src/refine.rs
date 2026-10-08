//! Transcript refinement (`transcript.refine`).
//!
//! A refinement Provider receives the current source transcript plus glossary
//! context and returns **correction proposals**. Proposals are candidate data:
//! they never write canonical Segments. The Desktop validates them, drops
//! anything that is not a conservative correction, stores the remainder for
//! human review and applies a proposal only through a revision-checked accept.

use crate::providers::ProviderConfig;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::time::Duration;

pub const REFINE_CONTRACT_VERSION: u32 = 1;

/// Maximum normalized character edit distance a proposal may have against the
/// original text. Larger rewrites are not "refinement" and are rejected.
pub const MAX_EDIT_RATIO: f64 = 0.4;

/// Segments sent to one refinement request. Keeps prompts small and lets a
/// failure affect only one batch.
pub const MAX_BATCH_SEGMENTS: usize = 40;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GlossaryTerm {
    pub id: i64,
    /// Canonical spelling, e.g. `秋山はるる`.
    pub term: String,
    /// Alternative spellings / romanizations / likely mis-recognitions.
    #[serde(default)]
    pub aliases: Vec<String>,
    #[serde(default)]
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RefineSegmentInput {
    pub segment_id: i64,
    pub revision: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RefineRequest {
    pub contract_version: u32,
    pub language: Option<String>,
    pub segments: Vec<RefineSegmentInput>,
    pub glossary: Vec<GlossaryTerm>,
}

/// One correction proposal as returned by a Provider (before validation).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RefineCandidate {
    pub segment_id: i64,
    pub text: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RefineResult {
    pub contract_version: u32,
    pub proposals: Vec<RefineCandidate>,
    #[serde(default)]
    pub notes: Vec<String>,
}

/// A validated proposal ready to be persisted for review.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ValidatedProposal {
    pub segment_id: i64,
    pub base_revision: i64,
    pub original_text: String,
    pub proposed_text: String,
    pub reason: String,
    pub confidence: Option<f64>,
    pub edit_ratio: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DiffOp {
    /// `equal`, `insert` or `delete`.
    pub kind: String,
    pub text: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct ValidationReport {
    pub accepted: Vec<ValidatedProposal>,
    pub notes: Vec<String>,
}

pub fn build_request(
    language: Option<String>,
    segments: Vec<RefineSegmentInput>,
    glossary: Vec<GlossaryTerm>,
) -> RefineRequest {
    RefineRequest {
        contract_version: REFINE_CONTRACT_VERSION,
        language,
        segments,
        glossary,
    }
}

pub const SYSTEM_PROMPT: &str = "You correct speech-recognition errors in ASMR / voice-drama transcripts.\n\
Rules:\n\
- Fix only clear recognition mistakes: misheard words, wrong homophones/kanji, broken punctuation, glossary names.\n\
- Never translate, summarize, paraphrase, censor, merge or split lines.\n\
- Keep the original language, register, fillers and onomatopoeia.\n\
- Use the glossary's canonical spelling when an alias or likely mis-hearing appears.\n\
- If a line is already correct, do not return it.\n\
Respond with JSON only: {\"proposals\":[{\"segment_id\":<id>,\"text\":\"<corrected line>\",\"reason\":\"<short reason>\",\"confidence\":<0..1>}]}";

/// Builds the user message for an OpenAI-compatible chat completion.
pub fn build_user_message(request: &RefineRequest) -> String {
    let glossary = request
        .glossary
        .iter()
        .map(|term| {
            json!({
                "term": term.term,
                "aliases": term.aliases,
                "note": term.note,
            })
        })
        .collect::<Vec<_>>();
    let lines = request
        .segments
        .iter()
        .map(|segment| json!({ "segment_id": segment.segment_id, "text": segment.text }))
        .collect::<Vec<_>>();
    json!({
        "language": request.language,
        "glossary": glossary,
        "lines": lines,
    })
    .to_string()
}

/// Extracts the refinement JSON from a model message. Models frequently wrap
/// JSON in prose or code fences; we take the outermost object.
pub fn parse_model_content(content: &str) -> Result<RefineResult, String> {
    let start = content
        .find('{')
        .ok_or_else(|| "refine provider returned no JSON object".to_string())?;
    let end = content
        .rfind('}')
        .ok_or_else(|| "refine provider returned no JSON object".to_string())?;
    if end < start {
        return Err("refine provider returned malformed JSON".into());
    }
    let value: Value = serde_json::from_str(&content[start..=end])
        .map_err(|e| format!("refine provider returned invalid JSON: {e}"))?;
    let proposals = value
        .get("proposals")
        .and_then(Value::as_array)
        .ok_or_else(|| "refine JSON has no proposals array".to_string())?;

    let mut out = Vec::new();
    let mut notes = Vec::new();
    for item in proposals {
        let segment_id = item.get("segment_id").and_then(|v| {
            v.as_i64()
                .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
        });
        let text = item.get("text").and_then(Value::as_str);
        match (segment_id, text) {
            (Some(segment_id), Some(text)) => out.push(RefineCandidate {
                segment_id,
                text: text.to_string(),
                reason: item
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string(),
                confidence: item.get("confidence").and_then(Value::as_f64),
            }),
            _ => notes.push("Ignored a proposal without segment_id/text.".to_string()),
        }
    }

    Ok(RefineResult {
        contract_version: REFINE_CONTRACT_VERSION,
        proposals: out,
        notes,
    })
}

/// Validates candidates against the request. Only conservative, real changes
/// to known Segments survive. Never fails the whole batch for one bad item.
pub fn validate_proposals(request: &RefineRequest, result: &RefineResult) -> ValidationReport {
    let by_id: HashMap<i64, &RefineSegmentInput> = request
        .segments
        .iter()
        .map(|segment| (segment.segment_id, segment))
        .collect();
    let mut report = ValidationReport {
        accepted: Vec::new(),
        notes: result.notes.clone(),
    };
    let mut seen = std::collections::HashSet::new();

    for candidate in &result.proposals {
        let Some(segment) = by_id.get(&candidate.segment_id) else {
            report.notes.push(format!(
                "Dropped proposal for unknown segment {}.",
                candidate.segment_id
            ));
            continue;
        };
        if !seen.insert(candidate.segment_id) {
            report.notes.push(format!(
                "Dropped duplicate proposal for segment {}.",
                candidate.segment_id
            ));
            continue;
        }
        let proposed = candidate.text.trim();
        if proposed.is_empty() {
            report.notes.push(format!(
                "Dropped empty proposal for segment {}.",
                candidate.segment_id
            ));
            continue;
        }
        if proposed == segment.text.trim() {
            continue;
        }
        // Glossary substitutions are expected corrections; measure the rest.
        let normalized = apply_glossary(segment.text.trim(), &request.glossary);
        let ratio = edit_ratio(&normalized, proposed);
        if ratio > MAX_EDIT_RATIO {
            report.notes.push(format!(
                "Dropped non-conservative rewrite for segment {} (edit ratio {:.2}).",
                candidate.segment_id, ratio
            ));
            continue;
        }
        let confidence = candidate
            .confidence
            .filter(|value| value.is_finite())
            .map(|value| value.clamp(0.0, 1.0));
        report.accepted.push(ValidatedProposal {
            segment_id: segment.segment_id,
            base_revision: segment.revision,
            original_text: segment.text.clone(),
            proposed_text: proposed.to_string(),
            reason: candidate.reason.trim().to_string(),
            confidence,
            edit_ratio: ratio,
        });
    }
    report
}

/// Replaces every glossary alias with its canonical term. Longer aliases are
/// applied first so overlapping aliases resolve deterministically. ASCII
/// aliases match case-insensitively (romanized names are typed freely).
pub fn apply_glossary(text: &str, glossary: &[GlossaryTerm]) -> String {
    let mut pairs: Vec<(&str, &str)> = glossary
        .iter()
        .flat_map(|term| {
            term.aliases
                .iter()
                .filter(|alias| !alias.trim().is_empty())
                .map(move |alias| (alias.trim(), term.term.as_str()))
        })
        .collect();
    pairs.sort_by(|a, b| b.0.chars().count().cmp(&a.0.chars().count()));
    let mut out = text.to_string();
    for (alias, canonical) in pairs {
        if alias.is_ascii() {
            let lower = out.to_ascii_lowercase();
            let needle = alias.to_ascii_lowercase();
            let mut result = String::with_capacity(out.len());
            let mut last = 0;
            for (index, _) in lower.match_indices(&needle) {
                result.push_str(&out[last..index]);
                result.push_str(canonical);
                last = index + needle.len();
            }
            result.push_str(&out[last..]);
            out = result;
        } else {
            out = out.replace(alias, canonical);
        }
    }
    out
}

/// Character-level Levenshtein distance divided by the longer length.
pub fn edit_ratio(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 0.0;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()] as f64 / longest as f64
}

/// Character-level diff (LCS) for review UI. Adjacent ops are coalesced.
pub fn char_diff(original: &str, proposed: &str) -> Vec<DiffOp> {
    let a: Vec<char> = original.chars().collect();
    let b: Vec<char> = proposed.chars().collect();
    let (n, m) = (a.len(), b.len());
    let mut lcs = vec![vec![0u32; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if a[i] == b[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }

    let mut ops: Vec<DiffOp> = Vec::new();
    let mut push = |kind: &str, ch: char| {
        if let Some(last) = ops.last_mut() {
            if last.kind == kind {
                last.text.push(ch);
                return;
            }
        }
        ops.push(DiffOp {
            kind: kind.to_string(),
            text: ch.to_string(),
        });
    };
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if a[i] == b[j] {
            push("equal", a[i]);
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            push("delete", a[i]);
            i += 1;
        } else {
            push("insert", b[j]);
            j += 1;
        }
    }
    while i < n {
        push("delete", a[i]);
        i += 1;
    }
    while j < m {
        push("insert", b[j]);
        j += 1;
    }
    ops
}

fn client() -> Result<Client, String> {
    Client::builder()
        .timeout(Duration::from_secs(120))
        .user_agent("Tsubame/0.1 TranscriptRefine")
        .build()
        .map_err(|e| e.to_string())
}

/// Calls an OpenAI-compatible `/chat/completions` endpoint. The same code path
/// serves paid APIs and local servers (LM Studio, Ollama, llama.cpp): there is
/// no online/local branch and no model-name switch.
pub async fn run_openai_compatible(
    provider: &ProviderConfig,
    secret: Option<&str>,
    model_id: &str,
    request: &RefineRequest,
) -> Result<RefineResult, String> {
    let url = format!(
        "{}/chat/completions",
        provider.base_url.trim_end_matches('/')
    );
    let body = json!({
        "model": model_id,
        "temperature": 0,
        "messages": [
            { "role": "system", "content": SYSTEM_PROMPT },
            { "role": "user", "content": build_user_message(request) },
        ],
    });

    let mut http = client()?.post(url).json(&body);
    match provider.auth_mode.as_str() {
        "none" => {}
        "bearer" => {
            let secret = secret.ok_or_else(|| "provider API key is not configured".to_string())?;
            http = http.bearer_auth(secret);
        }
        other => return Err(format!("unsupported provider auth mode: {other}")),
    }

    let response = http.send().await.map_err(|e| e.to_string())?;
    let status = response.status();
    let text = response.text().await.map_err(|e| e.to_string())?;
    if !status.is_success() {
        return Err(format!("refine provider returned HTTP {status}"));
    }
    let value: Value = serde_json::from_str(&text)
        .map_err(|e| format!("refine provider returned invalid JSON: {e}"))?;
    let content = value
        .pointer("/choices/0/message/content")
        .and_then(Value::as_str)
        .ok_or_else(|| "refine provider response has no message content".to_string())?;
    parse_model_content(content)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(id: i64, text: &str) -> RefineSegmentInput {
        RefineSegmentInput {
            segment_id: id,
            revision: 3,
            start_ms: id * 1000,
            end_ms: id * 1000 + 900,
            text: text.into(),
        }
    }

    fn request() -> RefineRequest {
        build_request(
            Some("ja".into()),
            vec![
                seg(1, "こんばんわ、今日も一日お疲れさま。"),
                seg(2, "あきやまはるるです。"),
                seg(3, "ゆっくり深呼吸して。"),
            ],
            vec![GlossaryTerm {
                id: 1,
                term: "秋山はるる".into(),
                aliases: vec!["Haruru Akiyama".into(), "あきやまはるる".into()],
                note: "voice actress".into(),
            }],
        )
    }

    #[test]
    fn parses_fenced_json_and_string_ids() {
        let content = "Here you go:\n```json\n{\"proposals\":[{\"segment_id\":\"2\",\"text\":\"秋山はるるです。\",\"reason\":\"glossary\",\"confidence\":0.9}]}\n```";
        let result = parse_model_content(content).unwrap();
        assert_eq!(result.proposals.len(), 1);
        assert_eq!(result.proposals[0].segment_id, 2);
        assert_eq!(result.proposals[0].confidence, Some(0.9));
    }

    #[test]
    fn rejects_content_without_json() {
        assert!(parse_model_content("no changes needed").is_err());
    }

    #[test]
    fn keeps_conservative_fixes_and_drops_rewrites() {
        let result = RefineResult {
            contract_version: 1,
            proposals: vec![
                RefineCandidate {
                    segment_id: 1,
                    text: "こんばんは、今日も一日お疲れさま。".into(),
                    reason: "particle".into(),
                    confidence: Some(1.7),
                },
                RefineCandidate {
                    segment_id: 2,
                    text: "秋山はるるです。".into(),
                    reason: "glossary".into(),
                    confidence: None,
                },
                RefineCandidate {
                    segment_id: 3,
                    text: "Take a slow, deep breath for me.".into(),
                    reason: "translated".into(),
                    confidence: Some(0.4),
                },
                RefineCandidate {
                    segment_id: 99,
                    text: "ghost".into(),
                    reason: String::new(),
                    confidence: None,
                },
            ],
            notes: vec![],
        };
        let report = validate_proposals(&request(), &result);
        let ids: Vec<i64> = report.accepted.iter().map(|p| p.segment_id).collect();
        assert_eq!(ids, vec![1, 2]);
        assert_eq!(report.accepted[0].confidence, Some(1.0));
        assert_eq!(report.accepted[0].base_revision, 3);
        assert!(report.notes.iter().any(|n| n.contains("non-conservative")));
        assert!(report
            .notes
            .iter()
            .any(|n| n.contains("unknown segment 99")));
    }

    #[test]
    fn unchanged_and_duplicate_proposals_are_ignored() {
        let result = RefineResult {
            contract_version: 1,
            proposals: vec![
                RefineCandidate {
                    segment_id: 3,
                    text: " ゆっくり深呼吸して。 ".into(),
                    reason: String::new(),
                    confidence: None,
                },
                RefineCandidate {
                    segment_id: 1,
                    text: "こんばんは、今日も一日お疲れさま。".into(),
                    reason: String::new(),
                    confidence: None,
                },
                RefineCandidate {
                    segment_id: 1,
                    text: "こんばんは。今日も一日お疲れさま。".into(),
                    reason: String::new(),
                    confidence: None,
                },
            ],
            notes: vec![],
        };
        let report = validate_proposals(&request(), &result);
        assert_eq!(report.accepted.len(), 1);
        assert!(report.notes.iter().any(|n| n.contains("duplicate")));
    }

    #[test]
    fn glossary_fixes_do_not_count_as_rewrites() {
        let mut req = request();
        req.segments[1].text = "Haruru Akiyamaです。よろしくね。".into();
        let result = RefineResult {
            contract_version: 1,
            proposals: vec![RefineCandidate {
                segment_id: 2,
                text: "秋山はるるです。よろしくね。".into(),
                reason: "glossary".into(),
                confidence: Some(0.95),
            }],
            notes: vec![],
        };
        let report = validate_proposals(&req, &result);
        assert_eq!(report.accepted.len(), 1);
        assert_eq!(report.accepted[0].edit_ratio, 0.0);
        assert_eq!(
            apply_glossary("HARURU akiyama!", &req.glossary),
            "秋山はるる!"
        );
    }

    #[test]
    fn edit_ratio_counts_characters_not_bytes() {
        assert_eq!(edit_ratio("", ""), 0.0);
        assert!((edit_ratio("こんばんわ", "こんばんは") - 0.2).abs() < 1e-9);
        assert_eq!(edit_ratio("abc", "xyz"), 1.0);
    }

    #[test]
    fn diff_marks_replacements() {
        let ops = char_diff("こんばんわ", "こんばんは");
        assert_eq!(
            ops,
            vec![
                DiffOp {
                    kind: "equal".into(),
                    text: "こんばん".into()
                },
                DiffOp {
                    kind: "delete".into(),
                    text: "わ".into()
                },
                DiffOp {
                    kind: "insert".into(),
                    text: "は".into()
                },
            ]
        );
        let rebuilt: String = ops
            .iter()
            .filter(|op| op.kind != "delete")
            .map(|op| op.text.as_str())
            .collect();
        assert_eq!(rebuilt, "こんばんは");
    }

    #[test]
    fn user_message_carries_glossary_and_ids() {
        let message = build_user_message(&request());
        let value: Value = serde_json::from_str(&message).unwrap();
        assert_eq!(value["glossary"][0]["term"], "秋山はるる");
        assert_eq!(value["lines"][1]["segment_id"], 2);
        assert_eq!(value["language"], "ja");
    }
}
