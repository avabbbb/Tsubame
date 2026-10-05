use crate::db::SegmentDraft;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtitleFormat {
    Srt,
    Vtt,
}

impl SubtitleFormat {
    pub fn from_path(path: &str) -> Result<Self, String> {
        let lower = path.to_ascii_lowercase();
        if lower.ends_with(".srt") {
            Ok(Self::Srt)
        } else if lower.ends_with(".vtt") {
            Ok(Self::Vtt)
        } else {
            Err("unsupported subtitle format; expected .srt or .vtt".into())
        }
    }
}

fn parse_timestamp(input: &str) -> Result<i64, String> {
    let clean = input.trim().replace(',', ".");
    let parts: Vec<&str> = clean.split(':').collect();

    let (hours, minutes, sec_part) = match parts.as_slice() {
        [m, s] => (0_i64, *m, *s),
        [h, m, s] => (
            h.parse::<i64>().map_err(|_| "invalid hours")?,
            *m,
            *s,
        ),
        _ => return Err(format!("invalid timestamp: {input}")),
    };

    let minutes = minutes
        .parse::<i64>()
        .map_err(|_| format!("invalid minutes: {input}"))?;

    let mut seconds_parts = sec_part.split('.');
    let seconds = seconds_parts
        .next()
        .ok_or_else(|| format!("invalid seconds: {input}"))?
        .parse::<i64>()
        .map_err(|_| format!("invalid seconds: {input}"))?;
    let millis_raw = seconds_parts.next().unwrap_or("0");
    if seconds_parts.next().is_some() {
        return Err(format!("invalid timestamp: {input}"));
    }

    let millis = match millis_raw.len() {
        0 => 0,
        1 => millis_raw.parse::<i64>().unwrap_or(0) * 100,
        2 => millis_raw.parse::<i64>().unwrap_or(0) * 10,
        _ => millis_raw[..3.min(millis_raw.len())]
            .parse::<i64>()
            .map_err(|_| format!("invalid milliseconds: {input}"))?,
    };

    if !(0..60).contains(&minutes) || !(0..60).contains(&seconds) {
        return Err(format!("timestamp out of range: {input}"));
    }

    Ok(hours * 3_600_000 + minutes * 60_000 + seconds * 1000 + millis)
}

fn parse_timing_line(line: &str) -> Result<(i64, i64), String> {
    let (start, rest) = line
        .split_once("-->")
        .ok_or_else(|| format!("missing --> in timing line: {line}"))?;
    let end_token = rest
        .split_whitespace()
        .next()
        .ok_or_else(|| format!("missing end timestamp: {line}"))?;

    let start_ms = parse_timestamp(start)?;
    let end_ms = parse_timestamp(end_token)?;
    if end_ms <= start_ms {
        return Err(format!("subtitle cue end must be after start: {line}"));
    }
    Ok((start_ms, end_ms))
}

pub fn parse_subtitles(contents: &str, format: SubtitleFormat) -> Result<Vec<SegmentDraft>, String> {
    let normalized = contents
        .trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .replace('\r', "\n");

    let mut lines = normalized.lines().peekable();

    if format == SubtitleFormat::Vtt {
        let first = lines.next().unwrap_or("").trim();
        if !first.starts_with("WEBVTT") {
            return Err("WebVTT file must start with WEBVTT".into());
        }
        while matches!(lines.peek(), Some(line) if line.trim().is_empty()) {
            lines.next();
        }
    }

    let mut cues = Vec::new();

    while let Some(raw) = lines.next() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }

        if format == SubtitleFormat::Vtt
            && (line.starts_with("NOTE")
                || line.starts_with("STYLE")
                || line.starts_with("REGION"))
        {
            while let Some(next) = lines.peek() {
                if next.trim().is_empty() {
                    break;
                }
                lines.next();
            }
            continue;
        }

        let timing_line = if line.contains("-->") {
            line.to_string()
        } else {
            let next = lines
                .next()
                .ok_or_else(|| format!("subtitle identifier without timing: {line}"))?;
            if !next.contains("-->") {
                return Err(format!("expected timing line after subtitle identifier {line:?}"));
            }
            next.trim().to_string()
        };

        let (start_ms, end_ms) = parse_timing_line(&timing_line)?;
        let mut text_lines = Vec::new();

        while let Some(next) = lines.peek() {
            if next.trim().is_empty() {
                break;
            }
            text_lines.push(lines.next().unwrap_or_default().trim_end().to_string());
        }

        let text = text_lines.join("
").trim().to_string();
        if text.is_empty() {
            continue;
        }

        cues.push(SegmentDraft {
            start_ms,
            end_ms,
            source_text: text,
        });
    }

    if cues.is_empty() {
        return Err("no subtitle cues found".into());
    }

    cues.sort_by_key(|cue| cue.start_ms);
    Ok(cues)
}

fn format_timestamp(ms: i64, format: SubtitleFormat) -> String {
    let total = ms.max(0);
    let hours = total / 3_600_000;
    let minutes = (total % 3_600_000) / 60_000;
    let seconds = (total % 60_000) / 1000;
    let millis = total % 1000;
    match format {
        SubtitleFormat::Srt => format!("{hours:02}:{minutes:02}:{seconds:02},{millis:03}"),
        SubtitleFormat::Vtt => format!("{hours:02}:{minutes:02}:{seconds:02}.{millis:03}"),
    }
}

pub fn export_subtitles(rows: &[(i64, i64, String)], format: SubtitleFormat) -> String {
    let mut output = String::new();

    if format == SubtitleFormat::Vtt {
        output.push_str("WEBVTT\n\n");
    }

    for (index, (start_ms, end_ms, text)) in rows.iter().enumerate() {
        if format == SubtitleFormat::Srt {
            output.push_str(&(index + 1).to_string());
            output.push('\n');
        }
        output.push_str(&format_timestamp(*start_ms, format));
        output.push_str(" --> ");
        output.push_str(&format_timestamp(*end_ms, format));
        output.push('\n');
        output.push_str(text);
        output.push_str("\n\n");
    }

    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_srt() {
        let input = "1\n00:00:01,000 --> 00:00:02,250\nこんにちは\n\n2\n00:00:03,000 --> 00:00:04,000\n世界\n";
        let cues = parse_subtitles(input, SubtitleFormat::Srt).unwrap();
        assert_eq!(cues.len(), 2);
        assert_eq!(cues[0].start_ms, 1000);
        assert_eq!(cues[0].end_ms, 2250);
        assert_eq!(cues[0].source_text, "こんにちは");
    }

    #[test]
    fn parses_vtt_with_identifier_and_settings() {
        let input = "WEBVTT\n\nintro\n00:01.000 --> 00:02.500 align:start\nHello\n";
        let cues = parse_subtitles(input, SubtitleFormat::Vtt).unwrap();
        assert_eq!(cues.len(), 1);
        assert_eq!(cues[0].start_ms, 1000);
        assert_eq!(cues[0].end_ms, 2500);
    }

    #[test]
    fn exports_srt_and_vtt() {
        let rows = vec![(1000, 2250, "hello".to_string())];
        let srt = export_subtitles(&rows, SubtitleFormat::Srt);
        assert!(srt.contains("00:00:01,000 --> 00:00:02,250"));
        let vtt = export_subtitles(&rows, SubtitleFormat::Vtt);
        assert!(vtt.starts_with("WEBVTT"));
        assert!(vtt.contains("00:00:01.000 --> 00:00:02.250"));
    }
}
