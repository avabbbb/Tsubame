import type { Segment, SegmentDraft } from "./types";

export function segmentDraft(segment: Segment): SegmentDraft {
  return {
    source_text: segment.source_text,
    translated_text: segment.translated_text,
    start_ms: segment.start_ms,
    end_ms: segment.end_ms,
  };
}

export function formatEditorTime(ms: number): string {
  const safe = Math.max(0, Math.round(ms));
  const hours = Math.floor(safe / 3_600_000);
  const minutes = Math.floor((safe % 3_600_000) / 60_000);
  const seconds = Math.floor((safe % 60_000) / 1000);
  const millis = safe % 1000;
  return [
    String(hours).padStart(2, "0"),
    String(minutes).padStart(2, "0"),
    String(seconds).padStart(2, "0"),
  ].join(":") + "." + String(millis).padStart(3, "0");
}

export function parseEditorTime(value: string): number | null {
  const normalized = value.trim().replace(",", ".");
  const parts = normalized.split(":");
  if (parts.length !== 2 && parts.length !== 3) return null;

  const secondsPart = parts[parts.length - 1];
  if (!secondsPart) return null;
  const [secondsRaw, millisRaw = "0"] = secondsPart.split(".");
  if (!/^\d+$/.test(secondsRaw) || !/^\d{1,3}$/.test(millisRaw)) return null;

  const seconds = Number(secondsRaw);
  const minutes = Number(parts[parts.length - 2]);
  const hours = parts.length === 3 ? Number(parts[0]) : 0;
  if (![hours, minutes, seconds].every(Number.isFinite)) return null;
  if (minutes < 0 || minutes > 59 || seconds < 0 || seconds > 59 || hours < 0) {
    return null;
  }

  const millis = Number(millisRaw.padEnd(3, "0"));
  return (((hours * 60 + minutes) * 60 + seconds) * 1000) + millis;
}
