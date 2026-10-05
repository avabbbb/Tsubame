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

export interface ProviderConfig {
  id: string;
  name: string;
  kind: string;
  base_url: string;
  model_list_url: string;
  auth_mode: "bearer" | "none";
  secret_ref: string | null;
  enabled: boolean;
  last_refresh_at: string | null;
  last_error: string | null;
}

export interface ProviderPreset {
  id: string;
  name: string;
  kind: string;
  base_url: string;
  model_list_url: string;
  auth_mode: "bearer" | "none";
  note: string;
}

export interface ProviderInput {
  id: string | null;
  name: string;
  kind: string;
  base_url: string;
  model_list_url: string;
  auth_mode: "bearer" | "none";
  api_key: string | null;
  enabled: boolean;
}

export interface ProviderModel {
  provider_id: string;
  model_id: string;
  display_name: string;
  owned_by: string;
  available: boolean;
  discovered_capabilities: string[];
  manual_capabilities: string[] | null;
  effective_capabilities: string[];
  capability_source: "catalog" | "manual" | "unknown";
  last_seen_at: string | null;
}

export interface ProviderTestResult {
  ok: boolean;
  status: string;
  model_count: number;
  message: string;
}

export type RailSection =
  | "home"
  | "library"
  | "discover"
  | "downloads"
  | "studio"
  | "jobs"
  | "settings";

export type ThemePreference = "system" | "dark" | "light";
