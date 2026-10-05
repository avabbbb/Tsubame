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
}

export type RailSection =
  | "home"
  | "library"
  | "discover"
  | "downloads"
  | "studio"
  | "jobs";

export type ThemePreference = "system" | "dark" | "light";
