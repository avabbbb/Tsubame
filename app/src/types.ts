export interface MediaItem {
  id: number;
  path: string;
  title: string;
  media_type: "audio" | "video";
  duration_ms: number;
  playback_position: number;
  file_size: number;
  speed: number;
  volume: number;
}

export interface Segment {
  id: number;
  media_id: number;
  start_ms: number;
  end_ms: number;
  source_text: string;
  translated_text: string;
  ordinal: number;
  revision: number;
  transcript_provenance: string;
  asr_provenance: string;
  refine_provenance: string;
  translation_provenance: string;
  tts_provenance: string;
  dirty_translation: boolean;
  dirty_tts: boolean;
  dirty_mix: boolean;
  dirty_subtitle: boolean;
  reviewed: boolean;
}

export interface SegmentDraft {
  source_text: string;
  translated_text: string;
  start_ms: number;
  end_ms: number;
}

export type SubtitleTextMode = "source" | "translation" | "bilingual";

export type RailSection =
  | "home"
  | "library"
  | "discover"
  | "downloads"
  | "studio"
  | "jobs";

export type ThemePreference = "system" | "dark" | "light";
