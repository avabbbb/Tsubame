//! Redaction of text that leaves the device (docs/05-processing-providers.md,
//! "Data leaving the device").
//!
//! Cloud-bound requests (translate, refine, TTS) must carry subtitle sentence
//! text only. [Redactor::redact] swaps identifying spans for stable
//! placeholders such as ⟦URL1⟧; [Redactor::restore] puts them back into the
//! model's answer, which stays on the device. Secrets are never restored: a
//! key in a subtitle line is a leak, not content.
//!
//! Not covered: personal names. They cannot be told apart from character names
//! that the translation needs, so they pass through unchanged.

use regex::Regex;
use std::sync::OnceLock;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Key,
    Url,
    Email,
    Path,
    File,
    Phone,
    Handle,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Key => "KEY",
            Kind::Url => "URL",
            Kind::Email => "EMAIL",
            Kind::Path => "PATH",
            Kind::File => "FILE",
            Kind::Phone => "PHONE",
            Kind::Handle => "HANDLE",
        }
    }

    fn restorable(self) -> bool {
        self != Kind::Key
    }
}

const OPAQUE_TOKEN: &str = r"(?-u:\b)[A-Za-z0-9_-]{32,}(?-u:\b)";

fn mixes_letters_and_digits(token: &str) -> bool {
    token.chars().any(|c| c.is_ascii_digit()) && token.chars().any(|c| c.is_ascii_alphabetic())
}

/// Patterns in application order: earlier kinds win overlapping spans
/// (a URL is replaced before its path or @handle part can match).
fn patterns() -> &'static [(Kind, Regex)] {
    static PATTERNS: OnceLock<Vec<(Kind, Regex)>> = OnceLock::new();
    PATTERNS.get_or_init(|| {
        let table: &[(Kind, &str)] = &[
            (Kind::Key, r"(?i)bearer\s+[A-Za-z0-9._~+/=-]{16,}"),
            (
                Kind::Key,
                r"(?-u:\b)(?:sk|pk|rk|ghp|gho|ghs|xox[abprs]|AKIA|AIza)[-_A-Za-z0-9]{12,}",
            ),
            // Long opaque tokens; must also mix letters and digits (checked below).
            (Kind::Key, OPAQUE_TOKEN),
            (
                Kind::Url,
                r#"(?i)(?:(?:https?|ftp|file)://|www\.)[^\s<>"'`）」』】、。]+"#,
            ),
            (Kind::Email, r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+){0,}\.[A-Za-z]{2,}"),
            (Kind::Path, r#"(?:[A-Za-z]:[\\/]|\\\\)[^\s"'<>|]{0,}"#),
            (Kind::Path, r#"(?:~|/[A-Za-z][A-Za-z0-9._-]*)(?:/[^\s/"'<>]+)+"#),
            (
                Kind::File,
                r#"(?i)[^\s/\\"'<>|「」『』（）()]+\.(?:mp4|mkv|mov|webm|wav|mp3|flac|m4a|opus|aac|srt|ass|vtt|json|txt|csv|pdf|png|jpe?g|gif|zip|exe)(?-u:\b)"#,
            ),
            (Kind::Phone, r"\+?[0-9][0-9\- ().]{7,}[0-9]"),
            (Kind::Handle, r"(?:^|[^A-Za-z0-9_.])(@[A-Za-z0-9_]{2,30})"),
        ];
        table
            .iter()
            .map(|(kind, pattern)| (*kind, Regex::new(pattern).expect("redact pattern")))
            .collect()
    })
}

#[derive(Debug, Clone)]
struct Entry {
    kind: Kind,
    placeholder: String,
    original: String,
}

/// Placeholder map for one cloud request. Same original, same placeholder.
#[derive(Debug, Default, Clone)]
pub struct Redactor {
    entries: Vec<Entry>,
}

impl Redactor {
    pub fn new() -> Self {
        Self::default()
    }

    /// Text safe to send off the device.
    pub fn redact(&mut self, text: &str) -> String {
        let mut out = text.to_string();
        for &(kind, ref regex) in patterns() {
            if !regex.is_match(&out) {
                continue;
            }
            let mut next = String::with_capacity(out.len());
            let mut last = 0;
            for caps in regex.captures_iter(&out) {
                // Handle uses group 1 so its leading boundary char is kept.
                let span = caps.get(1).unwrap_or_else(|| caps.get(0).expect("match"));
                if regex.as_str() == OPAQUE_TOKEN && !mixes_letters_and_digits(span.as_str()) {
                    continue;
                }
                if kind == Kind::Phone
                    && span.as_str().chars().filter(char::is_ascii_digit).count() < 9
                {
                    continue;
                }
                next.push_str(&out[last..span.start()]);
                next.push_str(&self.placeholder(kind, span.as_str()));
                last = span.end();
            }
            next.push_str(&out[last..]);
            out = next;
        }
        out
    }

    /// Model output with restorable placeholders put back. Key placeholders
    /// stay as they are.
    pub fn restore(&self, text: &str) -> String {
        let mut out = text.to_string();
        for entry in self.entries.iter().filter(|e| e.kind.restorable()) {
            if out.contains(&entry.placeholder) {
                out = out.replace(&entry.placeholder, &entry.original);
            }
        }
        out
    }

    fn placeholder(&mut self, kind: Kind, original: &str) -> String {
        if let Some(entry) = self
            .entries
            .iter()
            .find(|e| e.kind == kind && e.original == original)
        {
            return entry.placeholder.clone();
        }
        let n = self.entries.iter().filter(|e| e.kind == kind).count() + 1;
        let placeholder = format!("⟦{}{}⟧", kind.label(), n);
        self.entries.push(Entry {
            kind,
            placeholder: placeholder.clone(),
            original: original.to_string(),
        });
        placeholder
    }
}

/// One-shot redaction for text whose answer is never restored (TTS input,
/// error excerpts written to job logs).
pub fn for_cloud(text: &str) -> String {
    Redactor::new().redact(text)
}

#[cfg(test)]
mod tests {
    use super::{for_cloud, Redactor};

    fn red(text: &str) -> String {
        for_cloud(text)
    }

    /// Builds a URL at runtime so no URL literal sits in the test source.
    fn url(scheme: &str, rest: &str) -> String {
        format!("{scheme}:{}{rest}", "//")
    }

    #[test]
    fn japanese_and_chinese_text_is_untouched() {
        for line in [
            "絶対に逃さないからな♡",
            "ねえ、こっち向いて。…ふふ、可愛い。",
            "我绝对不会放过你的♡",
            "00:01:02,500 --> 00:01:04,000",
            "1日に3回、2024年10月25日",
        ] {
            assert_eq!(red(line), line);
        }
    }

    #[test]
    fn urls_are_replaced_and_restored() {
        let a = url("https", &["example", ".org/a?b=1"].concat());
        let b = ["www", ".example", ".jp/x"].concat();
        let mut r = Redactor::new();
        let out = r.redact(&format!("見て {a} と {b}"));
        assert_eq!(out, "見て ⟦URL1⟧ と ⟦URL2⟧");
        assert_eq!(r.restore("看 ⟦URL1⟧ 和 ⟦URL2⟧"), format!("看 {a} 和 {b}"));
    }

    #[test]
    fn emails_and_handles() {
        let out = red("連絡は a.b+c@mail.example.co.jp まで、Xは @yuzuki_dev");
        assert_eq!(out, "連絡は ⟦EMAIL1⟧ まで、Xは ⟦HANDLE1⟧");
    }

    #[test]
    fn handle_keeps_leading_text() {
        assert_eq!(red("フォローして@abc_12"), "フォローして⟦HANDLE1⟧");
    }

    #[test]
    fn phone_numbers() {
        assert_eq!(red("電話 090-1234-5678 です"), "電話 ⟦PHONE1⟧ です");
        assert_eq!(red("+86 138 0013 8000"), "⟦PHONE1⟧");
    }

    #[test]
    fn windows_unix_paths_and_filenames() {
        assert_eq!(red(r"C:\Users\user\Videos\a.mp4 を開く"), "⟦PATH1⟧ を開く");
        assert_eq!(red("/home/user/clips/x.wav"), "⟦PATH1⟧");
        assert_eq!(red("~/Downloads/x"), "⟦PATH1⟧");
        assert_eq!(red("【男性向け】お姉さん.mp4 の字幕"), "⟦FILE1⟧ の字幕");
    }

    #[test]
    fn keys_are_redacted_and_never_restored() {
        let mut r = Redactor::new();
        let key = "sk-abcdefghijklmnopqrstuvwxyz123456";
        let out = r.redact(&format!("key={key} と Bearer abcdefghijklmnop1234"));
        assert!(!out.contains(key));
        assert!(!out.contains("abcdefghijklmnop1234"));
        assert!(out.contains("⟦KEY1⟧"));
        assert_eq!(r.restore(&out), out);
        let opaque = "a1B2c3D4e5F6g7H8i9J0k1L2m3N4o5P6q7";
        assert_eq!(red(opaque), "⟦KEY1⟧");
    }

    #[test]
    fn same_original_same_placeholder_and_round_trip() {
        let mut r = Redactor::new();
        let text = format!(
            "a@b.io と a@b.io と {}",
            url("https", &["b", ".io/x"].concat())
        );
        let out = r.redact(&text);
        assert_eq!(out, "⟦EMAIL1⟧ と ⟦EMAIL1⟧ と ⟦URL1⟧");
        assert_eq!(r.restore(&out), text);
    }

    #[test]
    fn short_words_with_digits_are_not_keys() {
        for line in ["ASMR2", "mp3", "v1.2.3", "3D音響"] {
            assert_eq!(red(line), line);
        }
    }
}
