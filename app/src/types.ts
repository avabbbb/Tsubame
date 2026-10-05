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
  asr_provenance: string | null;
  refine_provenance: string | null;
  translation_provenance: string | null;
  tts_provenance: string | null;
  translation_dirty: boolean;
  tts_dirty: boolean;
  mix_dirty: boolean;
  subtitle_dirty: boolean;
}

export type SegmentDraft = Pick<
  Segment,
  "start_ms" | "end_ms" | "source_text" | "translated_text"
>;

export type RailSection =
  | "home"
  | "library"
  | "discover"
  | "downloads"
  | "studio"
  | "jobs";

export type ThemePreference = "system" | "dark" | "light";
