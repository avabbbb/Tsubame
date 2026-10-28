"""Redaction of text that leaves the device via the edge-tts shim.

Mirrors app/src-tauri/src/redact.rs (same kinds, same application order, same
⟦KIND1⟧ placeholders) so a request that bypasses the Desktop adapter still
carries subtitle sentence text only. Speech output needs no restore. Personal
names are not covered: they cannot be told apart from character names.
"""

from __future__ import annotations

import re

_OPAQUE = re.compile(r"\b[A-Za-z0-9_-]{32,}\b")

# Application order: earlier kinds win overlapping spans.
PATTERNS: list[tuple[str, re.Pattern[str]]] = [
    ("KEY", re.compile(r"(?i)bearer\s+[A-Za-z0-9._~+/=-]{16,}")),
    ("KEY", re.compile(r"\b(?:sk|pk|rk|ghp|gho|ghs|xox[abprs]|AKIA|AIza)[-_A-Za-z0-9]{12,}")),
    ("KEY", _OPAQUE),
    ("URL", re.compile(r"(?i)(?:(?:https?|ftp|file)://|www\.)[^\s<>\"'`）」』】、。]+")),
    ("EMAIL", re.compile(r"[A-Za-z0-9._%+-]+@[A-Za-z0-9-]+(?:\.[A-Za-z0-9-]+)*\.[A-Za-z]{2,}")),
    ("PATH", re.compile(r"(?:[A-Za-z]:[\\/]|\\\\)[^\s\"'<>|]*")),
    ("PATH", re.compile(r"(?:~|/[A-Za-z][A-Za-z0-9._-]*)(?:/[^\s/\"'<>]+)+")),
    ("FILE", re.compile(
        r"(?i)[^\s/\\\"'<>|「」『』（）()]+\.(?:mp4|mkv|mov|webm|wav|mp3|flac|m4a|opus|aac|srt|ass|vtt|json|txt|csv|pdf|png|jpe?g|gif|zip|exe)\b")),
    ("PHONE", re.compile(r"\+?[0-9][0-9\- ().]{7,}[0-9]")),
    ("HANDLE", re.compile(r"(?<![\w.@])@[A-Za-z0-9_]{2,30}\b")),
]


def _mixes_letters_and_digits(token: str) -> bool:
    return any(c.isdigit() for c in token) and any(c.isascii() and c.isalpha() for c in token)


def for_cloud(text: str) -> str:
    """Text safe to send off the device; identifying spans become placeholders."""
    seen: dict[tuple[str, str], str] = {}
    counters: dict[str, int] = {}
    out = text
    for kind, pattern in PATTERNS:
        def swap(match: re.Match[str], kind: str = kind, pattern: re.Pattern[str] = pattern) -> str:
            span = match.group(0)
            if pattern is _OPAQUE and not _mixes_letters_and_digits(span):
                return span
            if kind == "PHONE" and sum(c.isdigit() for c in span) < 9:
                return span
            key = (kind, span)
            if key not in seen:
                counters[kind] = counters.get(kind, 0) + 1
                seen[key] = f"\u27e6{kind}{counters[kind]}\u27e7"
            return seen[key]
        out = pattern.sub(swap, out)
    return out
