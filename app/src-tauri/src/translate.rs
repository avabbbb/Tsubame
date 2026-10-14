//! Translation (`text.translate`).
//!
//! Rust builds per-batch translation requests from canonical Segments, calls an
//! OpenAI-compatible `/chat/completions` Provider through the shared transport
//! in `refine.rs`, validates the answer item by item and applies each surviving
//! translation through a revision-checked write.
//!
//! Lineage: batch limits (16 lines / 4000 chars), the neighbouring-context idea,
//! the split-in-half retry on truncated output and the dubbing-script prompt
//! rules are adapted from ASMR-Dubber `src/asmr_dubber/translation.py` and
//! `src/asmr_dubber/prompts/translation.md` (commit `2faea94f`).
//! Copyright (c) 2026 ASMR Dubber contributors, MIT License
//! (https://github.com/EveningStudy/asmr-dubber/blob/main/LICENSE).
//!
//! Evolutions over Dubber: no vendor-name branching (one OpenAI-compatible
//! path, adapters chosen by Provider kind), glossary terms in every request,
//! per-batch failure isolation, per-item validation instead of all-or-nothing,
//! revision-checked apply and protection of human-edited translations.

use crate::db::Segment;
use crate::jobs::ProgressSink;
use crate::refine::{apply_glossary, extract_json_object, ChatCompletion, GlossaryTerm};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet, VecDeque};
use std::future::Future;

pub const TRANSLATE_CONTRACT_VERSION: u32 = 1;
/// Batch limits (docs/19): at most 16 sentences or 4000 source characters.
pub const MAX_BATCH_SEGMENTS: usize = 16;
pub const MAX_BATCH_CHARS: usize = 4000;
/// Neighbouring lines sent before and after a batch, for reference only.
pub const CONTEXT_LINES: usize = 4;
pub const TARGET_LANGUAGES: &[&str] = &["en", "zh-Hans", "zh-Hant"];
/// Provenance prefix of machine translations: `translate:<provider>:<model>`.
pub const PROVENANCE_PREFIX: &str = "translate:";
/// A translation longer than `factor × source chars + slack` is runaway output
/// (usually merged lines or a looping model).
pub const OVERLONG_FACTOR: usize = 4;
pub const OVERLONG_SLACK: usize = 40;

pub fn normalize_target_language(value: &str) -> Result<&'static str, String> {
    let value = value.trim();
    TARGET_LANGUAGES
        .iter()
        .copied()
        .find(|known| known.eq_ignore_ascii_case(value))
        .ok_or_else(|| {
            format!(
                "unsupported target language {value:?}; expected one of {}",
                TARGET_LANGUAGES.join(", ")
            )
        })
}

fn language_name(tag: &str) -> &str {
    match tag {
        "en" => "English",
        "zh-Hans" => "Simplified Chinese",
        "zh-Hant" => "Traditional Chinese",
        "ja" => "Japanese",
        "ko" => "Korean",
        other => other,
    }
}

/// A translation is human-owned unless it was written by `text.translate`.
///
/// `update_segment` (the Segment editor / Inspector) stamps a typed translation
/// with `translation_provenance = "human"`. `manual`, an empty provenance on a
/// non-empty translation (legacy rows) and imported text are all treated as
/// human: only `translate:*` rows may be overwritten without the explicit
/// `overwrite_human_edits` flag.
pub fn is_human_translation(segment: &Segment) -> bool {
    !segment.translated_text.trim().is_empty()
        && !segment
            .translation_provenance
            .starts_with(PROVENANCE_PREFIX)
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct Selection {
    pub targets: Vec<Segment>,
    /// Human-edited translations left alone.
    pub protected: Vec<i64>,
    /// Requested ids that do not belong to the media.
    pub unknown: Vec<i64>,
    pub notes: Vec<String>,
}

/// Chooses which Segments to translate.
///
/// - explicit `segment_ids`: exactly those (re-translation);
/// - otherwise: Segments whose translation is dirty, or empty and never
///   machine-translated (an empty `translate:*` row is a deliberate filler).
///
/// Empty source lines are never sent. Human translations are protected unless
/// `overwrite_human_edits` is set.
pub fn select_targets(
    segments: &[Segment],
    segment_ids: Option<&[i64]>,
    overwrite_human_edits: bool,
) -> Selection {
    let mut selection = Selection::default();
    if let Some(ids) = segment_ids {
        let known: HashSet<i64> = segments.iter().map(|s| s.id).collect();
        selection.unknown = ids
            .iter()
            .copied()
            .filter(|id| !known.contains(id))
            .collect();
        if !selection.unknown.is_empty() {
            selection.notes.push(format!(
                "Ignored {} segment id(s) that are not part of this media.",
                selection.unknown.len()
            ));
        }
    }
    let wanted: Option<HashSet<i64>> = segment_ids.map(|ids| ids.iter().copied().collect());
    for segment in segments {
        if let Some(wanted) = &wanted {
            if !wanted.contains(&segment.id) {
                continue;
            }
        } else {
            let pending = segment.dirty_translation
                || (segment.translated_text.trim().is_empty()
                    && !segment
                        .translation_provenance
                        .starts_with(PROVENANCE_PREFIX));
            if !pending {
                continue;
            }
        }
        if segment.source_text.trim().is_empty() {
            continue;
        }
        if is_human_translation(segment) && !overwrite_human_edits {
            selection.protected.push(segment.id);
            continue;
        }
        selection.targets.push(segment.clone());
    }
    if !selection.protected.is_empty() {
        selection.notes.push(format!(
            "Kept {} human-edited translation(s).",
            selection.protected.len()
        ));
    }
    selection
}

/// Splits targets into batches of at most `MAX_BATCH_SEGMENTS` lines and
/// `MAX_BATCH_CHARS` source characters (a single longer line is its own batch).
pub fn plan_batches(targets: &[Segment]) -> Vec<Vec<Segment>> {
    let mut batches = Vec::new();
    let mut current: Vec<Segment> = Vec::new();
    let mut chars = 0usize;
    for segment in targets {
        let length = segment.source_text.chars().count();
        if !current.is_empty()
            && (current.len() >= MAX_BATCH_SEGMENTS || chars + length > MAX_BATCH_CHARS)
        {
            batches.push(std::mem::take(&mut current));
            chars = 0;
        }
        chars += length;
        current.push(segment.clone());
    }
    if !current.is_empty() {
        batches.push(current);
    }
    batches
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranslateItem {
    pub id: i64,
    pub revision: i64,
    pub source_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ContextLine {
    pub id: i64,
    pub source_text: String,
    /// Existing translation, if any (helps keep wording consistent).
    pub translated_text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TranslateRequest {
    pub contract_version: u32,
    pub source_language: Option<String>,
    pub target_language: String,
    pub items: Vec<TranslateItem>,
    pub context_before: Vec<ContextLine>,
    pub context_after: Vec<ContextLine>,
    pub glossary: Vec<GlossaryTerm>,
}

/// Builds the request for one batch. `all_segments` is the whole media in
/// timeline order; context lines are the `CONTEXT_LINES` lines just outside
/// the batch's first/last line. Source text is glossary-normalized.
pub fn build_request(
    all_segments: &[Segment],
    batch: &[Segment],
    source_language: Option<String>,
    target_language: &str,
    glossary: &[GlossaryTerm],
) -> TranslateRequest {
    let positions: HashMap<i64, usize> = all_segments
        .iter()
        .enumerate()
        .map(|(index, segment)| (segment.id, index))
        .collect();
    let batch_ids: HashSet<i64> = batch.iter().map(|segment| segment.id).collect();
    let indices: Vec<usize> = batch
        .iter()
        .filter_map(|segment| positions.get(&segment.id).copied())
        .collect();
    let context_line = |segment: &Segment| ContextLine {
        id: segment.id,
        source_text: apply_glossary(segment.source_text.trim(), glossary),
        translated_text: segment.translated_text.trim().to_string(),
    };
    let (context_before, context_after) = match (indices.iter().min(), indices.iter().max()) {
        (Some(&first), Some(&last)) => {
            let before = all_segments[first.saturating_sub(CONTEXT_LINES)..first]
                .iter()
                .filter(|segment| !batch_ids.contains(&segment.id))
                .filter(|segment| !segment.source_text.trim().is_empty())
                .map(context_line)
                .collect();
            let end = (last + 1 + CONTEXT_LINES).min(all_segments.len());
            let after = all_segments[last + 1..end]
                .iter()
                .filter(|segment| !batch_ids.contains(&segment.id))
                .filter(|segment| !segment.source_text.trim().is_empty())
                .map(context_line)
                .collect();
            (before, after)
        }
        _ => (Vec::new(), Vec::new()),
    };
    TranslateRequest {
        contract_version: TRANSLATE_CONTRACT_VERSION,
        source_language,
        target_language: target_language.to_string(),
        items: batch
            .iter()
            .map(|segment| TranslateItem {
                id: segment.id,
                revision: segment.revision,
                source_text: apply_glossary(segment.source_text.trim(), glossary),
            })
            .collect(),
        context_before,
        context_after,
        glossary: glossary.to_vec(),
    }
}

/// System prompt for a target language (adapted from Dubber
/// `prompts/translation.md`, see file header).
pub fn system_prompt(source_language: Option<&str>, target_language: &str) -> String {
    let source = source_language
        .map(language_name)
        .unwrap_or("the source language");
    let target = language_name(target_language);
    format!(
        "You translate {source} ASMR, voice-drama and audio-play dialogue into {target} as a voice-over dub script.\n\
Rules:\n\
1. Return exactly one item per input id with the same id. Never merge, split, drop, reorder or invent ids.\n\
2. Translate faithfully and naturally for speaking aloud. Keep forms of address, politeness, tone and names consistent. Keep each line about as short as the original: the dub plays at natural speed.\n\
3. Glossary terms are canonical names; keep them consistent and use a term's note when it gives the preferred rendering.\n\
4. If a line is only laughter, crying, breathing, kissing, moaning, sound effects, stage directions or meaningless drawn-out sounds, its text must be an empty string. Never render such sounds as words.\n\
5. If non-verbal sounds are mixed with real speech, translate only the speech and drop meaningless fillers; keep short words that answer, deny, confirm, call out or express surprise.\n\
6. Do not soften, censor, explain or annotate whispered, intimate or adult content.\n\
7. context_before and context_after are neighbouring lines for reference only. Never translate or return them.\n\
8. Output no notes, brackets, sound-effect tags, romanization or source-language text.\n\
Respond with strict JSON only: {{\"translations\":[{{\"id\":<id>,\"text\":\"<{target} line, or empty string>\"}}]}}"
    )
}

/// Builds the user message for one batch.
pub fn build_user_message(request: &TranslateRequest) -> String {
    let glossary = request
        .glossary
        .iter()
        .map(|term| json!({ "term": term.term, "aliases": term.aliases, "note": term.note }))
        .collect::<Vec<_>>();
    let context = |lines: &[ContextLine]| {
        lines
            .iter()
            .map(|line| {
                json!({ "id": line.id, "source": line.source_text, "translation": line.translated_text })
            })
            .collect::<Vec<_>>()
    };
    let lines = request
        .items
        .iter()
        .map(|item| json!({ "id": item.id, "source": item.source_text }))
        .collect::<Vec<_>>();
    json!({
        "source_language": request.source_language,
        "target_language": request.target_language,
        "glossary": glossary,
        "context_before": context(&request.context_before),
        "lines": lines,
        "context_after": context(&request.context_after),
    })
    .to_string()
}

/// One raw `{id, text}` pair from a Provider answer.
#[derive(Debug, Clone, PartialEq)]
pub struct RawTranslation {
    pub id: Option<i64>,
    pub text: Option<String>,
}

/// Parses the model content (fenced JSON or JSON inside prose is accepted).
pub fn parse_model_content(content: &str) -> Result<Vec<RawTranslation>, String> {
    let value = extract_json_object(content, "translate")?;
    let items = value
        .get("translations")
        .and_then(Value::as_array)
        .ok_or_else(|| "translate JSON has no translations array".to_string())?;
    Ok(items
        .iter()
        .map(|item| RawTranslation {
            id: item.get("id").and_then(|v| {
                v.as_i64()
                    .or_else(|| v.as_str().and_then(|s| s.trim().parse().ok()))
            }),
            text: item.get("text").and_then(Value::as_str).map(str::to_string),
        })
        .collect())
}

/// A validated translation ready for a revision-checked apply.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TranslatedLine {
    pub segment_id: i64,
    pub base_revision: i64,
    /// Empty means "non-verbal filler, nothing to dub".
    pub text: String,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Validated {
    pub lines: Vec<TranslatedLine>,
    pub notes: Vec<String>,
    /// Ids whose translation was runaway output.
    pub overlong: Vec<i64>,
}

/// Validates parsed items against the request: unknown, duplicate and
/// missing/empty ids are dropped; items without a `text` string are dropped;
/// runaway (over-long) translations are reported in `overlong`.
pub fn validate(request: &TranslateRequest, parsed: &[RawTranslation]) -> Validated {
    let by_id: HashMap<i64, &TranslateItem> =
        request.items.iter().map(|item| (item.id, item)).collect();
    let mut seen = HashSet::new();
    let mut out = Validated::default();
    for raw in parsed {
        let Some(id) = raw.id else {
            out.notes
                .push("Dropped a translation without an id.".into());
            continue;
        };
        let Some(item) = by_id.get(&id) else {
            out.notes
                .push(format!("Dropped translation for unknown segment {id}."));
            continue;
        };
        if !seen.insert(id) {
            out.notes
                .push(format!("Dropped duplicate translation for segment {id}."));
            continue;
        }
        let Some(text) = raw.text.as_deref() else {
            out.notes.push(format!(
                "Dropped translation without text for segment {id}."
            ));
            continue;
        };
        let text = text.trim();
        let limit = item.source_text.chars().count() * OVERLONG_FACTOR + OVERLONG_SLACK;
        if text.chars().count() > limit {
            out.overlong.push(id);
            continue;
        }
        out.lines.push(TranslatedLine {
            segment_id: id,
            base_revision: item.revision,
            text: text.to_string(),
        });
    }
    let missing = request
        .items
        .iter()
        .filter(|item| !seen.contains(&item.id))
        .count();
    if missing > 0 {
        out.notes.push(format!(
            "{missing} line(s) were not returned by the provider and were left unchanged."
        ));
    }
    out
}

#[derive(Debug, Clone, PartialEq)]
pub enum BatchError {
    /// Output was cut off or ran away; splitting the batch may help.
    Truncated(String),
    Failed(String),
}

fn looks_truncated(content: &str) -> bool {
    let opens = content.matches('{').count();
    let closes = content.matches('}').count();
    opens > closes
}

/// Turns one completion into validated lines or a classified batch error.
pub fn interpret_completion(
    request: &TranslateRequest,
    completion: &ChatCompletion,
) -> Result<Validated, BatchError> {
    if completion.finish_reason.as_deref() == Some("length") {
        return Err(BatchError::Truncated(
            "provider output hit its length limit".into(),
        ));
    }
    let parsed = match parse_model_content(&completion.content) {
        Ok(parsed) => parsed,
        Err(error) if looks_truncated(&completion.content) => {
            return Err(BatchError::Truncated(error))
        }
        Err(error) => return Err(BatchError::Failed(error)),
    };
    let validated = validate(request, &parsed);
    if !validated.overlong.is_empty() && request.items.len() > 1 {
        return Err(BatchError::Truncated(format!(
            "runaway translation for segment(s) {:?}",
            validated.overlong
        )));
    }
    Ok(validated)
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct ApplyReport {
    pub applied: Vec<i64>,
    /// Same text as before; only provenance/dirty flag were updated.
    pub unchanged: Vec<i64>,
    /// Revision changed since the request was built; nothing written.
    pub stale: Vec<i64>,
    /// Human translation found at apply time; nothing written.
    pub protected: Vec<i64>,
    pub missing: Vec<i64>,
}

impl ApplyReport {
    fn merge(&mut self, other: ApplyReport) {
        self.applied.extend(other.applied);
        self.unchanged.extend(other.unchanged);
        self.stale.extend(other.stale);
        self.protected.extend(other.protected);
        self.missing.extend(other.missing);
    }
}

/// Revision-checked apply of one batch, in one transaction.
///
/// A changed translation writes `translated_text`, `translation_provenance`,
/// `dirty_translation=0`, `dirty_tts=1`, `dirty_mix=1`, `dirty_subtitle=1` and
/// bumps `revision`. An identical translation only records provenance and
/// clears `dirty_translation` (no revision bump, TTS stays valid).
pub fn apply_translations(
    conn: &mut Connection,
    lines: &[TranslatedLine],
    provenance: &str,
    overwrite_human_edits: bool,
) -> rusqlite::Result<ApplyReport> {
    let tx = conn.transaction()?;
    let mut report = ApplyReport::default();
    for line in lines {
        let current: Option<(i64, String, String)> = tx
            .query_row(
                "SELECT revision,translated_text,translation_provenance FROM segments WHERE id=?1",
                [line.segment_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()?;
        let Some((revision, translated, current_provenance)) = current else {
            report.missing.push(line.segment_id);
            continue;
        };
        if revision != line.base_revision {
            report.stale.push(line.segment_id);
            continue;
        }
        let human =
            !translated.trim().is_empty() && !current_provenance.starts_with(PROVENANCE_PREFIX);
        if human && !overwrite_human_edits {
            report.protected.push(line.segment_id);
            continue;
        }
        if translated == line.text {
            tx.execute(
                "UPDATE segments SET translation_provenance=?1, dirty_translation=0
                 WHERE id=?2 AND revision=?3",
                params![provenance, line.segment_id, line.base_revision],
            )?;
            report.unchanged.push(line.segment_id);
            continue;
        }
        let changed = tx.execute(
            "UPDATE segments
             SET translated_text=?1,
                 translation_provenance=?2,
                 dirty_translation=0,
                 dirty_tts=1,
                 dirty_mix=1,
                 dirty_subtitle=1,
                 revision=revision+1
             WHERE id=?3 AND revision=?4",
            params![line.text, provenance, line.segment_id, line.base_revision],
        )?;
        if changed == 0 {
            report.stale.push(line.segment_id);
        } else {
            report.applied.push(line.segment_id);
        }
    }
    tx.commit()?;
    Ok(report)
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct BatchFailure {
    pub segment_ids: Vec<i64>,
    pub error: String,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq)]
pub struct RunSummary {
    pub total: usize,
    pub done: usize,
    pub report: ApplyReport,
    pub failed_batches: Vec<BatchFailure>,
    pub cancelled: bool,
    /// Number of times a batch was split in half after truncated output.
    pub splits: usize,
    pub notes: Vec<String>,
}

/// Everything a run needs besides the transport and the database.
pub struct TranslatePlan {
    /// The whole media in timeline order (context source).
    pub all_segments: Vec<Segment>,
    pub batches: Vec<Vec<Segment>>,
    pub source_language: Option<String>,
    pub target_language: String,
    pub glossary: Vec<GlossaryTerm>,
}

/// Runs every batch in order. A batch that fails is recorded and the run
/// continues; truncated/runaway output splits the batch in half and retries
/// each half; a single line that still fails is recorded as a failed batch.
/// `apply` persists each successful batch immediately (checkpointing).
pub async fn run_translation<T, Fut, A>(
    plan: &TranslatePlan,
    mut transport: T,
    sink: &dyn ProgressSink,
    mut apply: A,
) -> RunSummary
where
    T: FnMut(String, String) -> Fut,
    Fut: Future<Output = Result<ChatCompletion, String>>,
    A: FnMut(&[TranslatedLine]) -> Result<ApplyReport, String>,
{
    let total = plan.batches.iter().map(Vec::len).sum();
    let mut summary = RunSummary {
        total,
        ..RunSummary::default()
    };
    let system = system_prompt(plan.source_language.as_deref(), &plan.target_language);
    let mut queue: VecDeque<Vec<Segment>> = plan.batches.iter().cloned().collect();
    sink.progress("translate", 0, total as u64);

    while let Some(batch) = queue.pop_front() {
        if sink.is_cancelled() {
            summary.cancelled = true;
            break;
        }
        let request = build_request(
            &plan.all_segments,
            &batch,
            plan.source_language.clone(),
            &plan.target_language,
            &plan.glossary,
        );
        let outcome = match transport(system.clone(), build_user_message(&request)).await {
            Ok(completion) => interpret_completion(&request, &completion),
            Err(error) => Err(BatchError::Failed(error)),
        };
        let ids = || batch.iter().map(|segment| segment.id).collect::<Vec<_>>();
        match outcome {
            Ok(validated) => {
                summary.notes.extend(validated.notes);
                for id in &validated.overlong {
                    summary
                        .notes
                        .push(format!("Dropped runaway translation for segment {id}."));
                }
                match apply(&validated.lines) {
                    Ok(report) => summary.report.merge(report),
                    Err(error) => summary.failed_batches.push(BatchFailure {
                        segment_ids: ids(),
                        error,
                    }),
                }
                summary.done += batch.len();
            }
            Err(BatchError::Truncated(reason)) if batch.len() > 1 => {
                summary.splits += 1;
                summary.notes.push(format!(
                    "Split a batch of {} lines in half and retried: {reason}",
                    batch.len()
                ));
                let mid = batch.len() / 2;
                let (left, right) = batch.split_at(mid);
                queue.push_front(right.to_vec());
                queue.push_front(left.to_vec());
            }
            Err(BatchError::Truncated(error)) | Err(BatchError::Failed(error)) => {
                summary.notes.push(format!(
                    "A batch of {} line(s) failed and was left unchanged: {error}",
                    batch.len()
                ));
                summary.failed_batches.push(BatchFailure {
                    segment_ids: ids(),
                    error,
                });
                summary.done += batch.len();
            }
        }
        sink.progress("translate", summary.done as u64, total as u64);
    }
    summary
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::MediaDb;
    use crate::jobs::NoopSink;
    use crate::subtitle::SubtitleCue;
    use std::cell::RefCell;
    use std::path::Path;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn seg(id: i64, text: &str) -> Segment {
        Segment {
            id,
            media_id: 1,
            start_ms: id * 1000,
            end_ms: id * 1000 + 900,
            source_text: text.into(),
            translated_text: String::new(),
            ordinal: id,
            revision: 0,
            transcript_provenance: "asr".into(),
            asr_provenance: "asr".into(),
            refine_provenance: String::new(),
            translation_provenance: String::new(),
            tts_provenance: String::new(),
            dirty_translation: true,
            dirty_tts: true,
            dirty_mix: true,
            dirty_subtitle: true,
            reviewed: false,
            asr_confidence: None,
            refine_confidence: None,
            align_provenance: String::new(),
            align_confidence: None,
            tts_asset_key: None,
            dub_skip: false,
            dub_pushed_ms: 0,
        }
    }

    fn segs(n: i64) -> Vec<Segment> {
        (1..=n).map(|id| seg(id, &format!("line {id}"))).collect()
    }

    fn glossary() -> Vec<GlossaryTerm> {
        vec![GlossaryTerm {
            id: 1,
            term: "秋山はるる".into(),
            aliases: vec!["あきやまはるる".into()],
            note: "CV; English: Haruru Akiyama".into(),
        }]
    }

    fn completion(content: &str) -> ChatCompletion {
        ChatCompletion {
            content: content.into(),
            finish_reason: Some("stop".into()),
        }
    }

    /// Mock provider: answers every line it was given with `T<id>`, or with a
    /// truncated completion when the batch has more than `max_lines` lines.
    fn echo_answer(user: &str, max_lines: usize) -> ChatCompletion {
        let value: Value = serde_json::from_str(user).unwrap();
        let lines = value["lines"].as_array().unwrap();
        if lines.len() > max_lines {
            return ChatCompletion {
                content: "{\"translations\":[{\"id\":1,\"text\":\"cut".into(),
                finish_reason: Some("length".into()),
            };
        }
        let items: Vec<Value> = lines
            .iter()
            .map(|line| json!({ "id": line["id"], "text": format!("T{}", line["id"]) }))
            .collect();
        completion(&json!({ "translations": items }).to_string())
    }

    fn plan(all: Vec<Segment>) -> TranslatePlan {
        let selection = select_targets(&all, None, false);
        TranslatePlan {
            batches: plan_batches(&selection.targets),
            all_segments: all,
            source_language: Some("ja".into()),
            target_language: "zh-Hans".into(),
            glossary: glossary(),
        }
    }

    fn block_on<F: Future>(future: F) -> F::Output {
        tauri::async_runtime::block_on(future)
    }

    #[test]
    fn batches_respect_line_and_char_limits() {
        let batches = plan_batches(&segs(40));
        assert_eq!(
            batches.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![16, 16, 8]
        );

        let long = "あ".repeat(1500);
        let rows: Vec<Segment> = (1..=5).map(|id| seg(id, &long)).collect();
        let batches = plan_batches(&rows);
        // 1500 chars each: two fit in 4000, a third would exceed it.
        assert_eq!(
            batches.iter().map(Vec::len).collect::<Vec<_>>(),
            vec![2, 2, 1]
        );

        let huge = vec![seg(1, &"x".repeat(5000)), seg(2, "short")];
        assert_eq!(plan_batches(&huge).len(), 2);
        assert!(plan_batches(&[]).is_empty());
    }

    #[test]
    fn context_window_surrounds_the_batch() {
        let all = segs(20);
        let request = build_request(&all, &all[8..12], None, "en", &[]);
        let before: Vec<i64> = request.context_before.iter().map(|l| l.id).collect();
        let after: Vec<i64> = request.context_after.iter().map(|l| l.id).collect();
        assert_eq!(before, vec![5, 6, 7, 8]);
        assert_eq!(after, vec![13, 14, 15, 16]);

        let edge = build_request(&all, &all[0..2], None, "en", &[]);
        assert!(edge.context_before.is_empty());
        assert_eq!(edge.context_after.len(), CONTEXT_LINES);
        let tail = build_request(&all, &all[18..20], None, "en", &[]);
        assert!(tail.context_after.is_empty());
    }

    #[test]
    fn context_carries_existing_translations_and_skips_batch_lines() {
        let mut all = segs(6);
        all[1].translated_text = "existing".into();
        // Non-contiguous selection: 1 and 4. Line 2-3 sit inside the span.
        let batch = vec![all[0].clone(), all[3].clone()];
        let request = build_request(&all, &batch, None, "en", &[]);
        assert!(request.context_before.is_empty());
        let after: Vec<i64> = request.context_after.iter().map(|l| l.id).collect();
        assert_eq!(after, vec![5, 6]);
        let request = build_request(&all, &[all[2].clone()], None, "en", &[]);
        assert_eq!(request.context_before[1].translated_text, "existing");
    }

    #[test]
    fn prompt_includes_glossary_and_normalizes_aliases() {
        let all = vec![seg(1, "あきやまはるるです。"), seg(2, "おやすみ。")];
        let request = build_request(&all, &all[..1], Some("ja".into()), "en", &glossary());
        assert_eq!(request.items[0].source_text, "秋山はるるです。");
        let message: Value = serde_json::from_str(&build_user_message(&request)).unwrap();
        assert_eq!(message["glossary"][0]["term"], "秋山はるる");
        assert_eq!(
            message["glossary"][0]["note"],
            "CV; English: Haruru Akiyama"
        );
        assert_eq!(message["lines"][0]["id"], 1);
        assert_eq!(message["context_after"][0]["source"], "おやすみ。");
        assert_eq!(message["target_language"], "en");
        let system = system_prompt(Some("ja"), "zh-Hant");
        assert!(system.contains("Japanese"));
        assert!(system.contains("Traditional Chinese"));
        assert!(system.contains("Glossary"));
        assert!(system.contains("\"translations\""));
    }

    #[test]
    fn target_languages_are_validated() {
        assert_eq!(normalize_target_language("zh-hans").unwrap(), "zh-Hans");
        assert_eq!(normalize_target_language(" EN ").unwrap(), "en");
        assert_eq!(normalize_target_language("zh-Hant").unwrap(), "zh-Hant");
        assert!(normalize_target_language("fr").is_err());
        assert!(normalize_target_language("zh").is_err());
    }

    #[test]
    fn parses_fenced_json_and_extra_prose() {
        let content = "Sure! Here is the translation:\n```json\n{\"translations\":[{\"id\":\"2\",\"text\":\"Good night.\"},{\"id\":3,\"text\":\"\"}]}\n```\nLet me know.";
        let parsed = parse_model_content(content).unwrap();
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, Some(2));
        assert_eq!(parsed[0].text.as_deref(), Some("Good night."));
        assert_eq!(parsed[1].text.as_deref(), Some(""));
        assert!(parse_model_content("I cannot help with that.").is_err());
        assert!(parse_model_content("{\"result\":[]}").is_err());
    }

    #[test]
    fn validation_drops_unknown_duplicate_and_empty_ids() {
        let all = segs(3);
        let request = build_request(&all, &all, None, "en", &[]);
        let parsed = parse_model_content(
            r#"{"translations":[
                {"id":1,"text":" One "},
                {"id":1,"text":"again"},
                {"id":99,"text":"ghost"},
                {"id":"","text":"no id"},
                {"text":"no id at all"},
                {"id":2},
                {"id":3,"text":""}
            ]}"#,
        )
        .unwrap();
        let validated = validate(&request, &parsed);
        let got: Vec<(i64, &str)> = validated
            .lines
            .iter()
            .map(|line| (line.segment_id, line.text.as_str()))
            .collect();
        assert_eq!(got, vec![(1, "One"), (3, "")]);
        let notes = validated.notes.join("\n");
        assert!(notes.contains("duplicate translation for segment 1"));
        assert!(notes.contains("unknown segment 99"));
        assert!(notes.contains("without an id"));
        assert!(notes.contains("without text for segment 2"));
        assert!(validated.notes.iter().all(|n| !n.contains("not returned")));
    }

    #[test]
    fn overlong_output_is_classified_as_truncated() {
        let all = segs(2);
        let request = build_request(&all, &all, None, "en", &[]);
        let runaway = "ha".repeat(200);
        let content = json!({"translations":[{"id":1,"text":runaway},{"id":2,"text":"ok"}]});
        let result = interpret_completion(&request, &completion(&content.to_string()));
        assert!(matches!(result, Err(BatchError::Truncated(_))));

        let single = build_request(&all, &all[..1], None, "en", &[]);
        let content = json!({"translations":[{"id":1,"text":runaway}]});
        let validated = interpret_completion(&single, &completion(&content.to_string())).unwrap();
        assert!(validated.lines.is_empty());
        assert_eq!(validated.overlong, vec![1]);

        let cut = ChatCompletion {
            content: "{\"translations\":[{\"id\":1,\"text\":\"a".into(),
            finish_reason: None,
        };
        assert!(matches!(
            interpret_completion(&request, &cut),
            Err(BatchError::Truncated(_))
        ));
        assert!(matches!(
            interpret_completion(&request, &completion("no json here")),
            Err(BatchError::Failed(_))
        ));
    }

    #[test]
    fn truncated_batches_split_in_half_until_they_fit() {
        let plan = plan(segs(16));
        let calls = Rc::new(RefCell::new(Vec::<usize>::new()));
        let seen = calls.clone();
        let applied = RefCell::new(Vec::new());
        let summary = block_on(run_translation(
            &plan,
            move |_system, user| {
                let value: Value = serde_json::from_str(&user).unwrap();
                seen.borrow_mut()
                    .push(value["lines"].as_array().unwrap().len());
                let answer = echo_answer(&user, 4);
                async move { Ok(answer) }
            },
            &NoopSink,
            |lines| {
                applied.borrow_mut().extend(lines.iter().cloned());
                Ok(ApplyReport {
                    applied: lines.iter().map(|l| l.segment_id).collect(),
                    ..ApplyReport::default()
                })
            },
        ));
        assert_eq!(*calls.borrow(), vec![16, 8, 4, 4, 8, 4, 4]);
        assert_eq!(summary.splits, 3);
        assert!(summary.failed_batches.is_empty());
        let ids: Vec<i64> = applied.borrow().iter().map(|l| l.segment_id).collect();
        assert_eq!(ids, (1..=16).collect::<Vec<_>>());
        assert_eq!(applied.borrow()[0].text, "T1");
        assert_eq!(summary.done, 16);
    }

    #[test]
    fn single_line_that_stays_truncated_fails_alone() {
        let plan = plan(segs(2));
        let summary = block_on(run_translation(
            &plan,
            |_system, user| {
                let answer = echo_answer(&user, 0);
                async move { Ok(answer) }
            },
            &NoopSink,
            |_| Ok(ApplyReport::default()),
        ));
        assert_eq!(summary.splits, 1);
        assert_eq!(summary.failed_batches.len(), 2);
        assert_eq!(summary.failed_batches[0].segment_ids, vec![1]);
        assert_eq!(summary.done, 2);
    }

    #[test]
    fn failed_batch_does_not_abort_others() {
        let plan = plan(segs(40));
        let call = AtomicUsize::new(0);
        let applied = RefCell::new(Vec::new());
        let summary = block_on(run_translation(
            &plan,
            |_system, user| {
                let n = call.fetch_add(1, Ordering::SeqCst);
                let answer = if n == 1 {
                    Err("provider returned HTTP 500".to_string())
                } else {
                    Ok(echo_answer(&user, 100))
                };
                async move { answer }
            },
            &NoopSink,
            |lines| {
                applied
                    .borrow_mut()
                    .extend(lines.iter().map(|l| l.segment_id));
                Ok(ApplyReport::default())
            },
        ));
        assert_eq!(summary.failed_batches.len(), 1);
        assert_eq!(
            summary.failed_batches[0].segment_ids,
            (17..=32).collect::<Vec<_>>()
        );
        assert!(summary.failed_batches[0].error.contains("HTTP 500"));
        assert_eq!(applied.borrow().len(), 24);
        assert_eq!(summary.done, 40);
        assert!(!summary.cancelled);
    }

    struct CancelAfter {
        calls: AtomicUsize,
        after: usize,
        cancelled: AtomicBool,
        last: std::sync::Mutex<(u64, u64)>,
    }

    impl ProgressSink for CancelAfter {
        fn progress(&self, _stage: &str, done: u64, total: u64) {
            *self.last.lock().unwrap() = (done, total);
        }
        fn is_cancelled(&self) -> bool {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n >= self.after {
                self.cancelled.store(true, Ordering::SeqCst);
            }
            self.cancelled.load(Ordering::SeqCst)
        }
    }

    #[test]
    fn cancellation_stops_before_the_next_batch_and_reports_progress() {
        let plan = plan(segs(40));
        let sink = CancelAfter {
            calls: AtomicUsize::new(0),
            after: 1,
            cancelled: AtomicBool::new(false),
            last: std::sync::Mutex::new((0, 0)),
        };
        let summary = block_on(run_translation(
            &plan,
            |_system, user| {
                let answer = echo_answer(&user, 100);
                async move { Ok(answer) }
            },
            &sink,
            |_| Ok(ApplyReport::default()),
        ));
        assert!(summary.cancelled);
        assert_eq!(summary.done, 16);
        assert_eq!(*sink.last.lock().unwrap(), (16, 40));
    }

    fn db_with_lines(texts: &[&str]) -> (MediaDb, Vec<Segment>) {
        let mut db = MediaDb::open(Path::new(":memory:")).unwrap();
        let media = db.import_media("example.mp3").unwrap();
        let cues: Vec<SubtitleCue> = texts
            .iter()
            .enumerate()
            .map(|(i, text)| SubtitleCue {
                start_ms: i as i64 * 1000,
                end_ms: i as i64 * 1000 + 800,
                text: (*text).into(),
            })
            .collect();
        let rows = db.replace_segments(media.id, &cues, "import:srt").unwrap();
        (db, rows)
    }

    #[test]
    fn apply_sets_text_provenance_dirty_flags_and_revision() {
        let (mut db, rows) = db_with_lines(&["おやすみ", "はぁ…"]);
        db.connection_mut()
            .execute(
                "UPDATE segments SET dirty_tts=0,dirty_mix=0,dirty_subtitle=0",
                [],
            )
            .unwrap();
        let lines = vec![
            TranslatedLine {
                segment_id: rows[0].id,
                base_revision: rows[0].revision,
                text: "Good night".into(),
            },
            TranslatedLine {
                segment_id: rows[1].id,
                base_revision: rows[1].revision,
                text: String::new(),
            },
        ];
        let report =
            apply_translations(db.connection_mut(), &lines, "translate:p:m", false).unwrap();
        assert_eq!(report.applied, vec![rows[0].id]);
        // Empty translation of an empty row: same text, provenance recorded only.
        assert_eq!(report.unchanged, vec![rows[1].id]);
        let first = db.get_segment(rows[0].id).unwrap().unwrap();
        assert_eq!(first.translated_text, "Good night");
        assert_eq!(first.translation_provenance, "translate:p:m");
        assert!(!first.dirty_translation);
        assert!(first.dirty_tts && first.dirty_mix && first.dirty_subtitle);
        assert_eq!(first.revision, rows[0].revision + 1);
        assert_eq!(first.source_text, "おやすみ");
        let filler = db.get_segment(rows[1].id).unwrap().unwrap();
        assert_eq!(filler.translation_provenance, "translate:p:m");
        assert!(!filler.dirty_translation);
        assert!(!filler.dirty_tts);
        assert_eq!(filler.revision, rows[1].revision);
        // A filler is not re-selected by a default run.
        let all = db.get_segments(rows[0].media_id).unwrap();
        assert!(select_targets(&all, None, false).targets.is_empty());
    }

    #[test]
    fn revision_mismatch_skips_the_line() {
        let (mut db, rows) = db_with_lines(&["おやすみ"]);
        let edited = db
            .update_segment(rows[0].id, rows[0].revision, "おやすみなさい", "", 0, 800)
            .unwrap()
            .unwrap();
        let lines = vec![TranslatedLine {
            segment_id: rows[0].id,
            base_revision: rows[0].revision,
            text: "Good night".into(),
        }];
        let report =
            apply_translations(db.connection_mut(), &lines, "translate:p:m", false).unwrap();
        assert_eq!(report.stale, vec![rows[0].id]);
        let row = db.get_segment(rows[0].id).unwrap().unwrap();
        assert_eq!(row, edited);
    }

    #[test]
    fn human_translations_are_protected_unless_overwrite_is_requested() {
        let (mut db, rows) = db_with_lines(&["おやすみ", "またね"]);
        // The Segment editor stamps typed translations as `human`.
        let human = db
            .update_segment(
                rows[0].id,
                rows[0].revision,
                "おやすみ",
                "Night night",
                0,
                800,
            )
            .unwrap()
            .unwrap();
        assert_eq!(human.translation_provenance, "human");
        assert!(is_human_translation(&human));

        // A clean human translation is simply not pending.
        let all = db.get_segments(rows[0].media_id).unwrap();
        let selection = select_targets(&all, None, false);
        assert!(selection.protected.is_empty());
        assert_eq!(
            selection.targets.iter().map(|s| s.id).collect::<Vec<_>>(),
            vec![rows[1].id]
        );
        // Editing the source dirties the translation but keeps it human-owned.
        let human = db
            .update_segment(
                human.id,
                human.revision,
                "おやすみなさい",
                "Night night",
                0,
                800,
            )
            .unwrap()
            .unwrap();
        assert!(human.dirty_translation);
        assert_eq!(human.translation_provenance, "human");
        let all = db.get_segments(rows[0].media_id).unwrap();
        let selection = select_targets(&all, None, false);
        assert_eq!(selection.protected, vec![rows[0].id]);
        assert!(selection.notes.iter().any(|n| n.contains("human-edited")));
        let explicit = select_targets(&all, Some(&[rows[0].id, 999]), false);
        assert!(explicit.targets.is_empty());
        assert_eq!(explicit.unknown, vec![999]);
        let forced = select_targets(&all, Some(&[rows[0].id]), true);
        assert_eq!(forced.targets.len(), 1);

        let line = vec![TranslatedLine {
            segment_id: human.id,
            base_revision: human.revision,
            text: "Good night".into(),
        }];
        let report =
            apply_translations(db.connection_mut(), &line, "translate:p:m", false).unwrap();
        assert_eq!(report.protected, vec![human.id]);
        assert_eq!(
            db.get_segment(human.id).unwrap().unwrap().translated_text,
            "Night night"
        );
        let report = apply_translations(db.connection_mut(), &line, "translate:p:m", true).unwrap();
        assert_eq!(report.applied, vec![human.id]);
        let row = db.get_segment(human.id).unwrap().unwrap();
        assert_eq!(row.translated_text, "Good night");
        assert_eq!(row.translation_provenance, "translate:p:m");
        assert!(!is_human_translation(&row));
    }

    #[test]
    fn legacy_and_manual_translations_count_as_human() {
        let mut row = seg(1, "a");
        row.translated_text = "x".into();
        for provenance in ["", "manual", "human", "import:srt"] {
            row.translation_provenance = provenance.into();
            assert!(is_human_translation(&row), "{provenance}");
        }
        row.translation_provenance = "translate:p:m".into();
        assert!(!is_human_translation(&row));
        row.translated_text = " ".into();
        row.translation_provenance = "human".into();
        assert!(!is_human_translation(&row));
    }

    #[test]
    fn default_selection_skips_clean_machine_translations_and_empty_sources() {
        let mut rows = segs(4);
        rows[0].translated_text = "done".into();
        rows[0].translation_provenance = "translate:p:m".into();
        rows[0].dirty_translation = false;
        rows[1].translated_text = "stale".into();
        rows[1].translation_provenance = "translate:p:m".into();
        rows[1].dirty_translation = true;
        rows[2].source_text = "  ".into();
        let ids: Vec<i64> = select_targets(&rows, None, false)
            .targets
            .iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(ids, vec![2, 4]);
        let explicit: Vec<i64> = select_targets(&rows, Some(&[1]), false)
            .targets
            .iter()
            .map(|s| s.id)
            .collect();
        assert_eq!(explicit, vec![1]);
    }

    #[test]
    fn end_to_end_with_mock_provider_and_database() {
        let (mut db, rows) = db_with_lines(&["一", "二", "三"]);
        let all = db.get_segments(rows[0].media_id).unwrap();
        let selection = select_targets(&all, None, false);
        let plan = TranslatePlan {
            batches: plan_batches(&selection.targets),
            all_segments: all,
            source_language: Some("ja".into()),
            target_language: "en".into(),
            glossary: vec![],
        };
        // Line 2 is edited by the user while the request is in flight.
        let edit_target = rows[1].clone();
        let db_cell = RefCell::new(&mut db);
        let summary = block_on(run_translation(
            &plan,
            |_system, user| {
                db_cell
                    .borrow_mut()
                    .update_segment(edit_target.id, edit_target.revision, "二つ", "", 1000, 1800)
                    .unwrap();
                let answer = echo_answer(&user, 100);
                async move { Ok(answer) }
            },
            &NoopSink,
            |lines| {
                apply_translations(
                    db_cell.borrow_mut().connection_mut(),
                    lines,
                    "translate:mock:m1",
                    false,
                )
                .map_err(|e| e.to_string())
            },
        ));
        assert_eq!(summary.report.applied, vec![rows[0].id, rows[2].id]);
        assert_eq!(summary.report.stale, vec![rows[1].id]);
        let first = db.get_segment(rows[0].id).unwrap().unwrap();
        assert_eq!(first.translated_text, format!("T{}", rows[0].id));
        let second = db.get_segment(rows[1].id).unwrap().unwrap();
        assert_eq!(second.translated_text, "");
        assert!(second.dirty_translation);
    }
}
