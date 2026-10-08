//! Forced-alignment contract (`speech.align`).
//!
//! An aligner receives media plus the current Segment text and returns refined
//! timings. Like ASR, an aligner never writes SQLite: the Desktop validates the
//! result against the Segments it was computed from and applies it as a
//! timing-only, revision-checked edit (TTS stays reusable).

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

pub const ALIGN_CONTRACT_VERSION: u32 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlignedWord {
    pub text: String,
    pub start_ms: i64,
    pub end_ms: i64,
    #[serde(default)]
    pub confidence: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlignedSegment {
    pub segment_id: i64,
    /// Revision of the Segment the aligner was given. Stale input is refused.
    pub base_revision: i64,
    pub start_ms: i64,
    pub end_ms: i64,
    #[serde(default)]
    pub confidence: Option<f64>,
    #[serde(default)]
    pub words: Vec<AlignedWord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AlignmentResult {
    pub contract_version: u32,
    pub provider_id: String,
    pub model_id: String,
    pub media_id: i64,
    pub segments: Vec<AlignedSegment>,
    #[serde(default)]
    pub notes: Vec<String>,
}

/// The current canonical state an alignment is checked against.
#[derive(Debug, Clone, PartialEq)]
pub struct AlignmentTarget {
    pub segment_id: i64,
    pub revision: i64,
}

/// Validates an alignment result. Any violation rejects the whole result: a
/// partially applied alignment would leave an inconsistent timeline.
pub fn validate(
    result: &AlignmentResult,
    media_id: i64,
    media_duration_ms: i64,
    targets: &[AlignmentTarget],
) -> Result<(), String> {
    if result.contract_version != ALIGN_CONTRACT_VERSION {
        return Err(format!(
            "unsupported alignment contract version {}",
            result.contract_version
        ));
    }
    if result.media_id != media_id {
        return Err("alignment result belongs to another media item".into());
    }
    if result.provider_id.trim().is_empty() || result.model_id.trim().is_empty() {
        return Err("alignment result must name its provider and model".into());
    }
    if result.segments.is_empty() {
        return Err("alignment result contains no segments".into());
    }

    let revisions: HashMap<i64, i64> = targets
        .iter()
        .map(|target| (target.segment_id, target.revision))
        .collect();
    let mut seen = HashSet::new();
    let mut previous_start = i64::MIN;

    for segment in &result.segments {
        let Some(revision) = revisions.get(&segment.segment_id) else {
            return Err(format!(
                "alignment references unknown segment {}",
                segment.segment_id
            ));
        };
        if *revision != segment.base_revision {
            return Err(format!(
                "segment {} changed since alignment started (revision {} != {})",
                segment.segment_id, revision, segment.base_revision
            ));
        }
        if !seen.insert(segment.segment_id) {
            return Err(format!(
                "alignment lists segment {} twice",
                segment.segment_id
            ));
        }
        if segment.start_ms < 0 || segment.end_ms <= segment.start_ms {
            return Err(format!("segment {} has invalid timing", segment.segment_id));
        }
        if media_duration_ms > 0 && segment.end_ms > media_duration_ms {
            return Err(format!(
                "segment {} ends after the media",
                segment.segment_id
            ));
        }
        if segment.start_ms < previous_start {
            return Err("aligned segments must be ordered by start time".into());
        }
        previous_start = segment.start_ms;

        if let Some(confidence) = segment.confidence {
            if !(0.0..=1.0).contains(&confidence) {
                return Err(format!(
                    "segment {} confidence must be within 0..1",
                    segment.segment_id
                ));
            }
        }
        let mut word_start = segment.start_ms;
        for word in &segment.words {
            if word.start_ms < word_start
                || word.end_ms < word.start_ms
                || word.end_ms > segment.end_ms
            {
                return Err(format!(
                    "segment {} has word timings outside the segment",
                    segment.segment_id
                ));
            }
            word_start = word.start_ms;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn result() -> AlignmentResult {
        AlignmentResult {
            contract_version: 1,
            provider_id: "local.aligner".into(),
            model_id: "mms-fa".into(),
            media_id: 7,
            segments: vec![
                AlignedSegment {
                    segment_id: 1,
                    base_revision: 2,
                    start_ms: 120,
                    end_ms: 1900,
                    confidence: Some(0.91),
                    words: vec![
                        AlignedWord {
                            text: "こんばんは".into(),
                            start_ms: 120,
                            end_ms: 800,
                            confidence: None,
                        },
                        AlignedWord {
                            text: "。".into(),
                            start_ms: 800,
                            end_ms: 900,
                            confidence: None,
                        },
                    ],
                },
                AlignedSegment {
                    segment_id: 2,
                    base_revision: 0,
                    start_ms: 2100,
                    end_ms: 3300,
                    confidence: None,
                    words: vec![],
                },
            ],
            notes: vec![],
        }
    }

    fn targets() -> Vec<AlignmentTarget> {
        vec![
            AlignmentTarget {
                segment_id: 1,
                revision: 2,
            },
            AlignmentTarget {
                segment_id: 2,
                revision: 0,
            },
        ]
    }

    #[test]
    fn accepts_valid_alignment() {
        assert!(validate(&result(), 7, 10_000, &targets()).is_ok());
    }

    #[test]
    fn refuses_stale_revision() {
        let mut stale = targets();
        stale[0].revision = 3;
        let err = validate(&result(), 7, 10_000, &stale).unwrap_err();
        assert!(err.contains("changed since alignment started"));
    }

    #[test]
    fn refuses_bad_timing_and_foreign_media() {
        let mut bad = result();
        bad.segments[1].end_ms = 2000;
        assert!(validate(&bad, 7, 10_000, &targets()).is_err());
        assert!(validate(&result(), 8, 10_000, &targets()).is_err());
        assert!(validate(&result(), 7, 3000, &targets()).is_err());
    }

    #[test]
    fn refuses_words_outside_segment() {
        let mut bad = result();
        bad.segments[0].words[1].end_ms = 5000;
        assert!(validate(&bad, 7, 10_000, &targets()).is_err());
    }

    #[test]
    fn refuses_unknown_or_duplicate_segments() {
        let mut bad = result();
        bad.segments[1].segment_id = 1;
        bad.segments[1].base_revision = 2;
        assert!(validate(&bad, 7, 10_000, &targets())
            .unwrap_err()
            .contains("twice"));
        let mut unknown = result();
        unknown.segments[1].segment_id = 42;
        assert!(validate(&unknown, 7, 10_000, &targets())
            .unwrap_err()
            .contains("unknown"));
    }
}
