//! `dub.place`: pure placement of dub clips on the original timeline.
//!
//! Contract: `docs/19-dubbing-pipeline.md` § dub.place.
//!
//! Product rules enforced here:
//! - a dub clip's duration is never changed (no speed-up, stretch or truncation);
//! - on collision the dub waits for the previous dub clip (+ `min_gap_ms`);
//! - drift beyond `max_drift_ms` is only flagged (`drift_exceeded`), never "fixed".
//!
//! The waiting chain follows the `sequential` timing mode of ASMR-Dubber
//! (`timing.py`, MIT, https://github.com/EveningStudy/asmr-dubber, commit
//! `2faea94f`), evolved with an end anchor, a minimum gap, a drift flag and an
//! incremental re-plan. No Dubber code is copied; the algorithm is re-implemented.

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use std::ops::Range;

pub const FLAG_DRIFT_EXCEEDED: &str = "drift_exceeded";
/// The Segment is marked `dub_skip`; nothing is placed.
pub const FLAG_SKIPPED: &str = "skipped";
/// The Segment has no synthesized clip (or a zero-length one); nothing is placed.
pub const FLAG_NO_CLIP: &str = "no_clip";

/// Bounds accepted for `beat_ms` (same ±30 s range Dubber allows for its offset).
pub const BEAT_LIMIT_MS: i64 = 30_000;
pub const MAX_MIN_GAP_MS: i64 = 60_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Anchor {
    Start,
    #[default]
    End,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlacementParams {
    pub anchor: Anchor,
    pub beat_ms: i64,
    pub min_gap_ms: i64,
    pub max_drift_ms: i64,
}

impl Default for PlacementParams {
    fn default() -> Self {
        Self {
            anchor: Anchor::End,
            beat_ms: 300,
            min_gap_ms: 120,
            max_drift_ms: 4_000,
        }
    }
}

impl PlacementParams {
    pub fn validate(&self) -> Result<(), String> {
        if !(-BEAT_LIMIT_MS..=BEAT_LIMIT_MS).contains(&self.beat_ms) {
            return Err(format!("beat_ms must be within ±{BEAT_LIMIT_MS} ms"));
        }
        if !(0..=MAX_MIN_GAP_MS).contains(&self.min_gap_ms) {
            return Err(format!("min_gap_ms must be within 0..={MAX_MIN_GAP_MS} ms"));
        }
        if self.max_drift_ms < 0 {
            return Err("max_drift_ms must be non-negative".to_string());
        }
        Ok(())
    }
}

/// One source sentence as seen by the placer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlacementSentence {
    pub segment_id: i64,
    pub src_start_ms: i64,
    pub src_end_ms: i64,
    /// Natural duration of the synthesized clip; `None` when not synthesized yet.
    pub clip_duration_ms: Option<i64>,
    pub skip: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Placement {
    pub segment_id: i64,
    /// For unplaced sentences (`skipped` / `no_clip`) start == end == the
    /// planned time, `drift_ms` is 0; consumers must check [`Placement::is_placed`].
    pub dub_start_ms: i64,
    pub dub_end_ms: i64,
    pub drift_ms: i64,
    pub flags: Vec<String>,
}

impl Placement {
    pub fn is_placed(&self) -> bool {
        !self
            .flags
            .iter()
            .any(|flag| flag == FLAG_SKIPPED || flag == FLAG_NO_CLIP)
    }

    pub fn drift_exceeded(&self) -> bool {
        self.flags.iter().any(|flag| flag == FLAG_DRIFT_EXCEEDED)
    }
}

fn validate_sentence(sentence: &PlacementSentence) -> Result<(), String> {
    if sentence.src_start_ms < 0 || sentence.src_end_ms < sentence.src_start_ms {
        return Err(format!(
            "segment {} has an invalid source window {}..{}",
            sentence.segment_id, sentence.src_start_ms, sentence.src_end_ms
        ));
    }
    if matches!(sentence.clip_duration_ms, Some(d) if d < 0) {
        return Err(format!(
            "segment {} has a negative clip duration",
            sentence.segment_id
        ));
    }
    Ok(())
}

fn planned_ms(sentence: &PlacementSentence, params: &PlacementParams) -> i64 {
    let anchor = match params.anchor {
        Anchor::Start => sentence.src_start_ms,
        Anchor::End => sentence.src_end_ms,
    };
    (anchor + params.beat_ms).max(0)
}

/// Places one sentence given the end of the previous placed dub clip.
/// Returns the placement and the carry (end of the last placed dub) after it.
fn place_one(
    sentence: &PlacementSentence,
    params: &PlacementParams,
    previous_dub_end: Option<i64>,
) -> (Placement, Option<i64>) {
    let planned = planned_ms(sentence, params);
    let unplaced = |flag: &str| Placement {
        segment_id: sentence.segment_id,
        dub_start_ms: planned,
        dub_end_ms: planned,
        drift_ms: 0,
        flags: vec![flag.to_string()],
    };
    if sentence.skip {
        return (unplaced(FLAG_SKIPPED), previous_dub_end);
    }
    let duration = match sentence.clip_duration_ms {
        Some(duration) if duration > 0 => duration,
        _ => return (unplaced(FLAG_NO_CLIP), previous_dub_end),
    };
    let start = match previous_dub_end {
        Some(end) => planned.max(end + params.min_gap_ms),
        None => planned,
    };
    let drift = start - planned;
    let mut flags = Vec::new();
    if drift > params.max_drift_ms {
        flags.push(FLAG_DRIFT_EXCEEDED.to_string());
    }
    let end = start + duration;
    (
        Placement {
            segment_id: sentence.segment_id,
            dub_start_ms: start,
            dub_end_ms: end,
            drift_ms: drift,
            flags,
        },
        Some(end),
    )
}

/// `dub.place`: full plan for ordered sentences.
pub fn place(
    sentences: &[PlacementSentence],
    params: &PlacementParams,
) -> Result<Vec<Placement>, String> {
    params.validate()?;
    let mut carry = None;
    let mut plan = Vec::with_capacity(sentences.len());
    for sentence in sentences {
        validate_sentence(sentence)?;
        let (placement, next) = place_one(sentence, params, carry);
        carry = next;
        plan.push(placement);
    }
    Ok(plan)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Replan {
    pub plan: Vec<Placement>,
    /// Indices that were actually recomputed; everything else was reused.
    pub recomputed: Range<usize>,
}

fn carry_before(plan: &[Placement], index: usize) -> Option<i64> {
    plan[..index]
        .iter()
        .rev()
        .find(|placement| placement.is_placed())
        .map(|placement| placement.dub_end_ms)
}

/// Incremental re-plan after the clip of `sentences[changed_index]` changed to
/// `new_duration` (`None` = clip removed). `sentences` and `previous_plan`
/// describe the state the previous plan was computed from (with the same params).
///
/// Recomputes from `changed_index` and stops as soon as the chain carry (end of
/// the last placed dub) equals the previous plan's carry at the same point: from
/// there on, the previous plan is reused unchanged. The result always equals
/// `place()` on the updated sentences.
pub fn replan(
    previous_plan: &[Placement],
    sentences: &[PlacementSentence],
    params: &PlacementParams,
    changed_index: usize,
    new_duration: Option<i64>,
) -> Result<Replan, String> {
    params.validate()?;
    if previous_plan.len() != sentences.len() {
        return Err("previous plan does not match the sentence list".to_string());
    }
    if changed_index >= sentences.len() {
        return Err("changed index out of range".to_string());
    }
    if previous_plan
        .iter()
        .zip(sentences)
        .any(|(placement, sentence)| placement.segment_id != sentence.segment_id)
    {
        return Err("previous plan does not match the sentence list".to_string());
    }

    let mut changed = sentences[changed_index].clone();
    changed.clip_duration_ms = new_duration;
    validate_sentence(&changed)?;

    let mut plan = previous_plan.to_vec();
    let mut new_carry = carry_before(previous_plan, changed_index);
    let mut old_carry = new_carry;
    let mut index = changed_index;
    while index < sentences.len() {
        if index > changed_index && new_carry == old_carry {
            break;
        }
        let sentence = if index == changed_index {
            &changed
        } else {
            &sentences[index]
        };
        validate_sentence(sentence)?;
        let (placement, next) = place_one(sentence, params, new_carry);
        new_carry = next;
        if previous_plan[index].is_placed() {
            old_carry = Some(previous_plan[index].dub_end_ms);
        }
        plan[index] = placement;
        index += 1;
    }
    Ok(Replan {
        plan,
        recomputed: changed_index..index,
    })
}

// ---------------------------------------------------------------------------
// DB reader (contract schema from the TTS track).
// ---------------------------------------------------------------------------

/// One Segment's dub inputs as stored by the TTS track:
/// `segments.dub_skip`, `segments.tts_asset_key` → `tts_assets(key, path, duration_ms, ...)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DubSentenceRow {
    pub sentence: PlacementSentence,
    pub tts_asset_key: Option<String>,
    pub tts_path: Option<String>,
}

fn has_column(conn: &Connection, table: &str, column: &str) -> rusqlite::Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = stmt
        .query_map([], |row| row.get::<_, String>(1))?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(columns.iter().any(|name| name == column))
}

fn has_table(conn: &Connection, table: &str) -> rusqlite::Result<bool> {
    conn.query_row(
        "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?1",
        params![table],
        |_| Ok(()),
    )
    .optional()
    .map(|row| row.is_some())
}

/// Reads placement inputs for one media item in timeline order
/// (`start_ms, ordinal, id`, the same order as `get_segments`).
///
/// Tolerates a database where the TTS track's columns/table are not migrated
/// yet: missing `dub_skip` reads as 0 and missing TTS data reads as "no clip".
pub fn load_dub_sentences(
    conn: &Connection,
    media_id: i64,
) -> rusqlite::Result<Vec<DubSentenceRow>> {
    let skip_expr = if has_column(conn, "segments", "dub_skip")? {
        "COALESCE(s.dub_skip, 0)"
    } else {
        "0"
    };
    let tts_ready =
        has_column(conn, "segments", "tts_asset_key")? && has_table(conn, "tts_assets")?;
    let sql = if tts_ready {
        format!(
            "SELECT s.id, s.start_ms, s.end_ms, {skip_expr}, s.tts_asset_key, t.path, t.duration_ms
               FROM segments s
               LEFT JOIN tts_assets t ON t.key = s.tts_asset_key
              WHERE s.media_id = ?1
              ORDER BY s.start_ms, s.ordinal, s.id"
        )
    } else {
        format!(
            "SELECT s.id, s.start_ms, s.end_ms, {skip_expr}, NULL, NULL, NULL
               FROM segments s
              WHERE s.media_id = ?1
              ORDER BY s.start_ms, s.ordinal, s.id"
        )
    };
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map(params![media_id], |row| {
        let key: Option<String> = row.get(4)?;
        let path: Option<String> = row.get(5)?;
        let duration: Option<i64> = row.get(6)?;
        // A key pointing at a missing asset row is "no clip", never a guess.
        let clip_duration_ms = if path.is_some() { duration } else { None };
        Ok(DubSentenceRow {
            sentence: PlacementSentence {
                segment_id: row.get(0)?,
                src_start_ms: row.get(1)?,
                src_end_ms: row.get(2)?,
                clip_duration_ms,
                skip: row.get::<_, i64>(3)? != 0,
            },
            tts_asset_key: key,
            tts_path: path,
        })
    })?;
    rows.collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DubPlan {
    pub media_id: i64,
    pub params: PlacementParams,
    pub placements: Vec<Placement>,
    pub placed_count: usize,
    pub drift_exceeded_count: usize,
}

/// Core of the `plan_dub` command (the Tauri wrapper only locks the DB).
pub fn plan_dub(
    conn: &Connection,
    media_id: i64,
    params: Option<PlacementParams>,
) -> Result<DubPlan, String> {
    let params = params.unwrap_or_default();
    let rows = load_dub_sentences(conn, media_id).map_err(|e| e.to_string())?;
    let sentences = rows.into_iter().map(|row| row.sentence).collect::<Vec<_>>();
    let placements = place(&sentences, &params)?;
    Ok(DubPlan {
        media_id,
        params,
        placed_count: placements.iter().filter(|p| p.is_placed()).count(),
        drift_exceeded_count: placements.iter().filter(|p| p.drift_exceeded()).count(),
        placements,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: i64, start: i64, end: i64, clip: Option<i64>) -> PlacementSentence {
        PlacementSentence {
            segment_id: id,
            src_start_ms: start,
            src_end_ms: end,
            clip_duration_ms: clip,
            skip: false,
        }
    }

    fn params() -> PlacementParams {
        PlacementParams::default()
    }

    #[test]
    fn defaults_match_contract() {
        let p = params();
        assert_eq!(p.anchor, Anchor::End);
        assert_eq!((p.beat_ms, p.min_gap_ms, p.max_drift_ms), (300, 120, 4000));
        let parsed: PlacementParams = serde_json::from_str(r#"{"anchor":"start"}"#).unwrap();
        assert_eq!(parsed.anchor, Anchor::Start);
        assert_eq!(parsed.beat_ms, 300);
    }

    #[test]
    fn anchor_end_places_one_beat_after_the_sentence() {
        let plan = place(&[s(1, 1000, 2500, Some(800))], &params()).unwrap();
        assert_eq!(plan[0].dub_start_ms, 2800);
        assert_eq!(plan[0].dub_end_ms, 3600);
        assert_eq!(plan[0].drift_ms, 0);
        assert!(plan[0].flags.is_empty());
    }

    #[test]
    fn anchor_start_uses_sentence_start() {
        let p = PlacementParams {
            anchor: Anchor::Start,
            beat_ms: 500,
            ..params()
        };
        let plan = place(&[s(1, 1000, 2500, Some(800))], &p).unwrap();
        assert_eq!(plan[0].dub_start_ms, 1500);
    }

    #[test]
    fn beat_is_configurable_and_clamped_at_zero() {
        let p = PlacementParams {
            beat_ms: -800,
            anchor: Anchor::Start,
            ..params()
        };
        let plan = place(&[s(1, 300, 900, Some(100))], &p).unwrap();
        assert_eq!(plan[0].dub_start_ms, 0);
        assert!(PlacementParams {
            beat_ms: 40_000,
            ..params()
        }
        .validate()
        .is_err());
    }

    #[test]
    fn collisions_wait_with_min_gap_and_chain() {
        // Planned: 1300, 1600, 1900. Clip 1 ends at 3300 → 2 waits to 3420,
        // ends 4420 → 3 waits to 4540.
        let plan = place(
            &[
                s(1, 0, 1000, Some(2000)),
                s(2, 1100, 1300, Some(1000)),
                s(3, 1400, 1600, Some(500)),
            ],
            &params(),
        )
        .unwrap();
        assert_eq!(plan[1].dub_start_ms, 3420);
        assert_eq!(plan[1].drift_ms, 3420 - 1600);
        assert_eq!(plan[2].dub_start_ms, 4540);
        assert_eq!(plan[2].drift_ms, 4540 - 1900);
        for pair in plan.windows(2) {
            assert!(pair[1].dub_start_ms >= pair[0].dub_end_ms + 120);
        }
    }

    #[test]
    fn min_gap_is_respected_exactly() {
        let p = PlacementParams {
            min_gap_ms: 0,
            ..params()
        };
        let plan = place(&[s(1, 0, 100, Some(1000)), s(2, 200, 300, Some(10))], &p).unwrap();
        assert_eq!(plan[1].dub_start_ms, plan[0].dub_end_ms);
    }

    #[test]
    fn natural_pause_absorbs_drift() {
        let plan = place(
            &[
                s(1, 0, 1000, Some(3000)),
                s(2, 1100, 1200, Some(500)),
                s(3, 10_000, 11_000, Some(500)),
            ],
            &params(),
        )
        .unwrap();
        assert!(plan[1].drift_ms > 0);
        assert_eq!(plan[2].drift_ms, 0);
        assert_eq!(plan[2].dub_start_ms, 11_300);
    }

    #[test]
    fn drift_beyond_limit_is_flagged_not_fixed() {
        let plan = place(
            &[s(1, 0, 100, Some(10_000)), s(2, 200, 300, Some(1000))],
            &params(),
        )
        .unwrap();
        assert!(plan[1].drift_exceeded());
        assert_eq!(plan[1].dub_end_ms - plan[1].dub_start_ms, 1000);
        // Exactly at the limit is not flagged (contract: drift > max).
        let p = PlacementParams {
            max_drift_ms: plan[1].drift_ms,
            ..params()
        };
        let plan = place(
            &[s(1, 0, 100, Some(10_000)), s(2, 200, 300, Some(1000))],
            &p,
        )
        .unwrap();
        assert!(!plan[1].drift_exceeded());
    }

    #[test]
    fn skipped_and_missing_clips_do_not_occupy_time() {
        let mut skipped = s(2, 1100, 1200, Some(5000));
        skipped.skip = true;
        let plan = place(
            &[
                s(1, 0, 1000, Some(500)),
                skipped,
                s(3, 1300, 1400, None),
                s(4, 1500, 1600, Some(0)),
                s(5, 1700, 1800, Some(400)),
            ],
            &params(),
        )
        .unwrap();
        assert_eq!(plan[1].flags, vec![FLAG_SKIPPED]);
        assert_eq!(plan[2].flags, vec![FLAG_NO_CLIP]);
        assert_eq!(plan[3].flags, vec![FLAG_NO_CLIP]);
        assert!(!plan[1].is_placed() && !plan[2].is_placed() && !plan[3].is_placed());
        assert_eq!(plan[1].dub_start_ms, plan[1].dub_end_ms);
        // 5 only waits on 1 (ends 1800): planned 2100 ≥ 1800 + 120.
        assert_eq!(plan[4].dub_start_ms, 2100);
        assert_eq!(plan[4].drift_ms, 0);
    }

    #[test]
    fn clip_durations_are_never_changed() {
        let sentences = pseudo_random_sentences(200, 7);
        let plan = place(&sentences, &params()).unwrap();
        for (placement, sentence) in plan.iter().zip(&sentences) {
            if placement.is_placed() {
                assert_eq!(
                    placement.dub_end_ms - placement.dub_start_ms,
                    sentence.clip_duration_ms.unwrap()
                );
                assert!(placement.drift_ms >= 0);
            }
        }
    }

    #[test]
    fn invalid_input_is_rejected() {
        assert!(place(&[s(1, 500, 100, Some(1))], &params()).is_err());
        assert!(place(&[s(1, 0, 100, Some(-1))], &params()).is_err());
        assert!(place(
            &[s(1, 0, 100, Some(1))],
            &PlacementParams {
                min_gap_ms: -1,
                ..params()
            }
        )
        .is_err());
    }

    /// Deterministic LCG so the property tests need no extra crate.
    fn pseudo_random_sentences(count: usize, seed: u64) -> Vec<PlacementSentence> {
        let mut state = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        let mut next = |modulus: u64| {
            state = state
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            (state >> 33) % modulus
        };
        let mut cursor = 0i64;
        (0..count)
            .map(|i| {
                cursor += next(3000) as i64;
                let start = cursor;
                let end = start + 200 + next(4000) as i64;
                cursor = end;
                let clip = match next(10) {
                    0 => None,
                    _ => Some(100 + next(6000) as i64),
                };
                PlacementSentence {
                    segment_id: i as i64 + 1,
                    src_start_ms: start,
                    src_end_ms: end,
                    clip_duration_ms: clip,
                    skip: next(12) == 0,
                }
            })
            .collect()
    }

    #[test]
    fn incremental_replan_equals_full_replan() {
        for seed in 0..40u64 {
            let sentences = pseudo_random_sentences(60, seed);
            let p = params();
            let before = place(&sentences, &p).unwrap();
            let index = (seed as usize * 7) % sentences.len();
            for new_duration in [None, Some(50), Some(2500), Some(12_000)] {
                let result = replan(&before, &sentences, &p, index, new_duration).unwrap();
                let mut updated = sentences.clone();
                updated[index].clip_duration_ms = new_duration;
                assert_eq!(result.plan, place(&updated, &p).unwrap(), "seed {seed}");
                assert_eq!(result.recomputed.start, index);
            }
        }
    }

    #[test]
    fn incremental_replan_stops_once_converged() {
        // 1 and 2 chain; 3 is far away (natural pause) so it is never recomputed.
        let sentences = vec![
            s(1, 0, 1000, Some(1500)),
            s(2, 1100, 1200, Some(500)),
            s(3, 20_000, 21_000, Some(500)),
            s(4, 21_100, 21_200, Some(500)),
        ];
        let p = params();
        let before = place(&sentences, &p).unwrap();
        let result = replan(&before, &sentences, &p, 0, Some(2500)).unwrap();
        // Sentence 3 is recomputed once (its carry-in changed) and the chain
        // converges there; sentence 4 is reused.
        assert_eq!(result.recomputed, 0..3);
        assert_eq!(result.plan[2..], before[2..]);
        assert!(result.plan[1].dub_start_ms > before[1].dub_start_ms);

        // Unchanged duration converges immediately after the changed sentence.
        let same = replan(&before, &sentences, &p, 1, Some(500)).unwrap();
        assert_eq!(same.recomputed, 1..2);
        assert_eq!(same.plan, before);
    }

    #[test]
    fn replan_rejects_mismatched_plan() {
        let sentences = vec![s(1, 0, 100, Some(10))];
        assert!(replan(&[], &sentences, &params(), 0, None).is_err());
        let plan = place(&sentences, &params()).unwrap();
        assert!(replan(&plan, &sentences, &params(), 3, None).is_err());
    }

    fn contract_db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE segments (id INTEGER PRIMARY KEY, media_id INTEGER, start_ms INTEGER,
               end_ms INTEGER, ordinal INTEGER DEFAULT 0, tts_asset_key TEXT,
               dub_skip INTEGER DEFAULT 0);
             CREATE TABLE tts_assets (key TEXT PRIMARY KEY, path TEXT, sha256 TEXT,
               duration_ms INTEGER, sample_rate INTEGER, channels INTEGER);
             INSERT INTO tts_assets VALUES ('k1','/a/1.wav','x',900,24000,1);
             INSERT INTO segments VALUES (10, 1, 5000, 6000, 1, NULL, 0);
             INSERT INTO segments VALUES (11, 1, 0, 1000, 0, 'k1', 0);
             INSERT INTO segments VALUES (12, 1, 7000, 8000, 2, 'k1', 1);
             INSERT INTO segments VALUES (13, 1, 9000, 9500, 3, 'missing', 0);
             INSERT INTO segments VALUES (14, 2, 0, 1000, 0, 'k1', 0);",
        )
        .unwrap();
        conn
    }

    #[test]
    fn db_reader_follows_contract_schema() {
        let conn = contract_db();
        let rows = load_dub_sentences(&conn, 1).unwrap();
        let ids = rows
            .iter()
            .map(|r| r.sentence.segment_id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec![11, 10, 12, 13]);
        assert_eq!(rows[0].sentence.clip_duration_ms, Some(900));
        assert_eq!(rows[0].tts_path.as_deref(), Some("/a/1.wav"));
        assert_eq!(rows[1].sentence.clip_duration_ms, None);
        assert!(rows[2].sentence.skip);
        assert_eq!(rows[3].sentence.clip_duration_ms, None);

        let plan = plan_dub(&conn, 1, None).unwrap();
        assert_eq!(plan.placed_count, 1);
        assert_eq!(plan.placements[0].dub_start_ms, 1300);
        assert_eq!(plan.placements[2].flags, vec![FLAG_SKIPPED]);
    }

    #[test]
    fn db_reader_tolerates_unmigrated_schema() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch(
            "CREATE TABLE segments (id INTEGER PRIMARY KEY, media_id INTEGER, start_ms INTEGER,
               end_ms INTEGER, ordinal INTEGER DEFAULT 0);
             INSERT INTO segments VALUES (1, 1, 0, 1000, 0);",
        )
        .unwrap();
        let rows = load_dub_sentences(&conn, 1).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sentence.clip_duration_ms, None);
        assert!(!rows[0].sentence.skip);
    }
}
