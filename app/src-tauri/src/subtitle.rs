use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubtitleCue {
    pub start_ms: i64,
    pub end_ms: i64,
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtitleFormat {
    Srt,
    Vtt,
    Lrc,
}

impl SubtitleFormat {
    pub fn from_path(path: &Path) -> Result<Self, String> {
        match path
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str()
        {
            "srt" => Ok(Self::Srt),
            "vtt" => Ok(Self::Vtt),
            "lrc" => Ok(Self::Lrc),
            other => Err(format!("unsupported subtitle format: {other}")),
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Self::Srt => "srt",
            Self::Vtt => "vtt",
            Self::Lrc => "lrc",
        }
    }
}

pub fn parse(content: &str, format: SubtitleFormat) -> Result<Vec<SubtitleCue>, String> {
    match format {
        SubtitleFormat::Srt => parse_srt(content),
        SubtitleFormat::Vtt => parse_vtt(content),
        SubtitleFormat::Lrc => parse_lrc(content),
    }
}

pub fn serialize(cues: &[SubtitleCue], format: SubtitleFormat) -> String {
    match format {
        SubtitleFormat::Srt => serialize_srt(cues),
        SubtitleFormat::Vtt => serialize_vtt(cues),
        SubtitleFormat::Lrc => serialize_lrc(cues),
    }
}

fn parse_srt(content: &str) -> Result<Vec<SubtitleCue>, String> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let mut cues = Vec::new();

    for block in normalized.split("\n\n") {
        let lines = block
            .lines()
            .map(str::trim_end)
            .filter(|line| !line.trim().is_empty())
            .collect::<Vec<_>>();
        if lines.is_empty() {
            continue;
        }

        let timing_index = lines
            .iter()
            .position(|line| line.contains("-->"))
            .ok_or_else(|| format!("SRT block is missing timing: {}", lines[0]))?;
        let (start_ms, end_ms) = parse_timing_line(lines[timing_index], ',')?;
        let text = lines[timing_index + 1..].join("\n").trim().to_string();
        if text.is_empty() {
            continue;
        }
        cues.push(SubtitleCue {
            start_ms,
            end_ms,
            text,
        });
    }

    validate_cues(cues)
}

fn parse_vtt(content: &str) -> Result<Vec<SubtitleCue>, String> {
    let normalized = content.replace("\r\n", "\n").replace('\r', "\n");
    let body = normalized
        .strip_prefix("\u{feff}")
        .unwrap_or(&normalized)
        .trim_start();

    if !body.starts_with("WEBVTT") {
        return Err("WebVTT file must begin with WEBVTT".to_string());
    }

    let mut cues = Vec::new();
    let mut lines = body.lines().peekable();
    let _ = lines.next();

    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if trimmed.starts_with("NOTE") {
            while let Some(next) = lines.peek() {
                if next.trim().is_empty() {
                    break;
                }
                lines.next();
            }
            continue;
        }
        if trimmed == "STYLE" || trimmed == "REGION" {
            while let Some(next) = lines.peek() {
                if next.trim().is_empty() {
                    break;
                }
                lines.next();
            }
            continue;
        }

        let timing_line = if trimmed.contains("-->") {
            trimmed.to_string()
        } else {
            let Some(next) = lines.next() else {
                break;
            };
            if !next.contains("-->") {
                continue;
            }
            next.trim().to_string()
        };

        let (start_ms, end_ms) = parse_timing_line(&timing_line, '.')?;
        let mut payload = Vec::new();
        while let Some(next) = lines.peek() {
            if next.trim().is_empty() {
                break;
            }
            payload.push(lines.next().unwrap_or_default().to_string());
        }

        let text = payload.join("\n").trim().to_string();
        if text.is_empty() {
            continue;
        }
        cues.push(SubtitleCue {
            start_ms,
            end_ms,
            text,
        });
    }

    validate_cues(cues)
}

fn parse_timing_line(line: &str, decimal: char) -> Result<(i64, i64), String> {
    let (start, rest) = line
        .split_once("-->")
        .ok_or_else(|| format!("invalid timing line: {line}"))?;
    let end = rest
        .split_whitespace()
        .next()
        .ok_or_else(|| format!("missing end timestamp: {line}"))?;
    let start_ms = parse_timestamp(start.trim(), decimal)?;
    let end_ms = parse_timestamp(end.trim(), decimal)?;
    if end_ms <= start_ms {
        return Err(format!("subtitle end must be after start: {line}"));
    }
    Ok((start_ms, end_ms))
}

fn parse_timestamp(value: &str, decimal: char) -> Result<i64, String> {
    let value = value.trim();
    let (whole, fraction) = value
        .rsplit_once(decimal)
        .ok_or_else(|| format!("timestamp lacks milliseconds: {value}"))?;
    if fraction.len() != 3 || !fraction.chars().all(|c| c.is_ascii_digit()) {
        return Err(format!("invalid millisecond precision: {value}"));
    }

    let parts = whole.split(':').collect::<Vec<_>>();
    let (hours, minutes, seconds) = match parts.as_slice() {
        [minutes, seconds] => (0_i64, parse_i64(minutes)?, parse_i64(seconds)?),
        [hours, minutes, seconds] => (parse_i64(hours)?, parse_i64(minutes)?, parse_i64(seconds)?),
        _ => return Err(format!("invalid timestamp: {value}")),
    };

    if minutes > 59 || seconds > 59 {
        return Err(format!("timestamp component out of range: {value}"));
    }

    let millis = parse_i64(fraction)?;
    Ok((((hours * 60 + minutes) * 60 + seconds) * 1000) + millis)
}

fn parse_i64(value: &str) -> Result<i64, String> {
    value
        .parse::<i64>()
        .map_err(|_| format!("invalid numeric timestamp component: {value}"))
}

fn validate_cues(mut cues: Vec<SubtitleCue>) -> Result<Vec<SubtitleCue>, String> {
    if cues.is_empty() {
        return Err("subtitle file contains no usable cues".to_string());
    }
    cues.sort_by_key(|cue| (cue.start_ms, cue.end_ms));
    Ok(cues)
}

fn serialize_srt(cues: &[SubtitleCue]) -> String {
    cues.iter()
        .enumerate()
        .map(|(index, cue)| {
            format!(
                "{}\n{} --> {}\n{}",
                index + 1,
                format_srt_timestamp(cue.start_ms),
                format_srt_timestamp(cue.end_ms),
                cue.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n")
        + "\n"
}

fn serialize_vtt(cues: &[SubtitleCue]) -> String {
    let body = cues
        .iter()
        .map(|cue| {
            format!(
                "{} --> {}\n{}",
                format_vtt_timestamp(cue.start_ms),
                format_vtt_timestamp(cue.end_ms),
                cue.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    format!("WEBVTT\n\n{body}\n")
}

fn format_srt_timestamp(ms: i64) -> String {
    format_timestamp(ms, ',')
}

fn format_vtt_timestamp(ms: i64) -> String {
    format_timestamp(ms, '.')
}

fn format_timestamp(ms: i64, decimal: char) -> String {
    let ms = ms.max(0);
    let hours = ms / 3_600_000;
    let minutes = (ms / 60_000) % 60;
    let seconds = (ms / 1000) % 60;
    let millis = ms % 1000;
    format!("{hours:02}:{minutes:02}:{seconds:02}{decimal}{millis:03}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_srt_and_roundtrips() {
        let source = "1\n00:00:01,250 --> 00:00:03,500\nおやすみ\n\n2\n00:00:04,000 --> 00:00:05,125\nまた明日\n";
        let cues = parse(source, SubtitleFormat::Srt).unwrap();
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start_ms, 1_250);
        assert_eq!(cues[1].end_ms, 5_125);
        let exported = serialize(&cues, SubtitleFormat::Srt);
        let reparsed = parse(&exported, SubtitleFormat::Srt).unwrap();
        assert_eq!(cues, reparsed);
    }

    #[test]
    fn parses_webvtt_identifier_and_settings() {
        let source = "WEBVTT\n\nintro\n00:01.000 --> 00:03.500 line:90%\nこんにちは\n\n00:04.000 --> 00:05.250\nさようなら\n";
        let cues = parse(source, SubtitleFormat::Vtt).unwrap();
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start_ms, 1_000);
        assert_eq!(cues[0].text, "こんにちは");
        let exported = serialize(&cues, SubtitleFormat::Vtt);
        assert!(exported.starts_with("WEBVTT\n\n"));
    }

    #[test]
    fn rejects_non_positive_duration() {
        let source = "1\n00:00:03,000 --> 00:00:03,000\ninvalid\n";
        assert!(parse(source, SubtitleFormat::Srt).is_err());
    }
}

/// How long the last LRC line lasts when no end marker follows it.
const LRC_LAST_CUE_MS: i64 = 5_000;

/// [mm:ss.xx]text lines, several stamps per line allowed; an empty line
/// after a stamp marks where the previous line ends. Tags like [ar:..] are
/// skipped. A cue lasts until the next later stamp.
fn parse_lrc(content: &str) -> Result<Vec<SubtitleCue>, String> {
    let mut marks: Vec<(i64, String)> = Vec::new();
    for line in content.lines() {
        let mut rest = line.trim().trim_start_matches('\u{feff}');
        let mut stamps = Vec::new();
        while let Some(tail) = rest.strip_prefix('[') {
            let Some(close) = tail.find(']') else { break };
            let Some(ms) = parse_lrc_time(&tail[..close]) else {
                break;
            };
            stamps.push(ms);
            rest = tail[close + 1..].trim_start();
        }
        let text = rest.trim();
        for ms in stamps {
            marks.push((ms, text.to_string()));
        }
    }
    marks.sort_by(|a, b| a.0.cmp(&b.0));
    let mut cues = Vec::new();
    for i in 0..marks.len() {
        let (start, text) = (marks[i].0, &marks[i].1);
        if text.is_empty() {
            continue;
        }
        let end = marks[i + 1..]
            .iter()
            .map(|mark| mark.0)
            .find(|&ms| ms > start)
            .unwrap_or(start + LRC_LAST_CUE_MS);
        cues.push(SubtitleCue {
            start_ms: start,
            end_ms: end,
            text: text.clone(),
        });
    }
    validate_cues(cues)
}

/// mm:ss, mm:ss.x, mm:ss.xx, mm:ss.xxx (also : before the fraction).
fn parse_lrc_time(tag: &str) -> Option<i64> {
    let (min, sec) = tag.split_once(':')?;
    let min: i64 = min.trim().parse().ok()?;
    let (whole, frac) = sec.split_once(['.', ':']).unwrap_or((sec, ""));
    let whole: i64 = whole.parse().ok()?;
    if !(0..60).contains(&whole) || frac.len() > 3 || !frac.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let frac_ms: i64 = format!("{frac:0<3}").parse().ok()?;
    Some(min.checked_mul(60_000)? + whole.checked_mul(1_000)? + frac_ms)
}

fn serialize_lrc(cues: &[SubtitleCue]) -> String {
    let mut out = String::new();
    for (i, cue) in cues.iter().enumerate() {
        out.push_str(&format!(
            "[{}]{}\n",
            format_lrc_timestamp(cue.start_ms),
            cue.text.replace('\n', " ")
        ));
        if cues
            .get(i + 1)
            .map_or(true, |next| next.start_ms > cue.end_ms)
        {
            out.push_str(&format!("[{}]\n", format_lrc_timestamp(cue.end_ms)));
        }
    }
    out
}

fn format_lrc_timestamp(ms: i64) -> String {
    let ms = ms.max(0);
    format!(
        "{:02}:{:02}.{:02}",
        ms / 60_000,
        ms / 1_000 % 60,
        ms % 1_000 / 10
    )
}

#[cfg(test)]
mod lrc_tests {
    use super::{parse, serialize, SubtitleCue, SubtitleFormat};

    fn cue(start_ms: i64, end_ms: i64, text: &str) -> SubtitleCue {
        SubtitleCue {
            start_ms,
            end_ms,
            text: text.into(),
        }
    }

    #[test]
    fn parses_stamps_end_markers_and_skips_tags() {
        let lrc = "\u{feff}[ar:CV]\n[ti:おやすみ]\n[00:01.50]おはよう\n[00:03.00][00:10.000]もう一回\n[00:04.2]\n[00:12]最後\n";
        let cues = parse(lrc, SubtitleFormat::Lrc).unwrap();
        assert_eq!(
            cues,
            [
                cue(1_500, 3_000, "おはよう"),
                cue(3_000, 4_200, "もう一回"),
                cue(10_000, 12_000, "もう一回"),
                cue(12_000, 17_000, "最後"),
            ]
        );
    }

    #[test]
    fn roundtrips_through_serialize() {
        let cues = vec![
            cue(1_500, 3_000, "a"),
            cue(3_000, 4_200, "b"),
            cue(10_000, 12_340, "c"),
        ];
        let text = serialize(&cues, SubtitleFormat::Lrc);
        assert_eq!(
            text,
            "[00:01.50]a\n[00:03.00]b\n[00:04.20]\n[00:10.00]c\n[00:12.34]\n"
        );
        assert_eq!(parse(&text, SubtitleFormat::Lrc).unwrap(), cues);
        assert!(parse("[ar:x]\nno stamps", SubtitleFormat::Lrc).is_err());
    }
}
