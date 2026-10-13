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
  asr_confidence: number | null;
  refine_confidence: number | null;
  align_provenance: string;
  align_confidence: number | null;
  /** Content-addressed key of the current speech.tts clip, if any. */
  tts_asset_key: string | null;
  /** Excluded from dub placement and mix. */
  dub_skip: boolean;
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
  execution: "remote_api" | "local_server" | "local_runtime" | "native_os";
  system_managed: boolean;
  availability: "ready" | "runtime-required" | "unsupported-platform" | "error";
  message: string;
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
  execution: "remote_api" | "local_server";
  base_url: string;
  model_list_url: string;
  auth_mode: "bearer" | "none";
  note: string;
}

export interface ProviderInput {
  id: string | null;
  name: string;
  kind: string;
  execution: "remote_api" | "local_server" | "local_runtime" | "native_os";
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


export interface CapabilityTarget {
  provider_id: string;
  provider_name: string;
  provider_kind: string;
  execution: "remote_api" | "local_server" | "local_runtime" | "native_os";
  system_managed: boolean;
  provider_availability: "ready" | "runtime-required" | "unsupported-platform" | "error";
  provider_message: string;
  model_id: string;
  display_name: string;
  available: boolean;
  effective_capabilities: string[];
}

export interface AsrSegmentCandidate {
  start_ms: number;
  end_ms: number;
  text: string;
  confidence: number | null;
}

export interface AsrResult {
  contract_version: number;
  adapter_id: string;
  model_id: string;
  language: string | null;
  segments: AsrSegmentCandidate[];
  notes: string[];
}

export interface AsrTranscriptionOutcome {
  result: AsrResult;
  segments: Segment[];
}

export interface GlossaryTerm {
  id: number;
  term: string;
  aliases: string[];
  note: string;
}

export interface DiffOp {
  kind: "equal" | "insert" | "delete";
  text: string;
}

export type RefineProposalStatus =
  | "pending"
  | "accepted"
  | "rejected"
  | "stale"
  | "superseded";

export interface RefineProposal {
  id: number;
  media_id: number;
  segment_id: number;
  base_revision: number;
  original_text: string;
  proposed_text: string;
  reason: string;
  confidence: number | null;
  edit_ratio: number;
  provenance: string;
  status: RefineProposalStatus;
  created_at: string;
  diff: DiffOp[];
}

export interface RefineOutcome {
  provenance: string;
  proposals: RefineProposal[];
  new_proposals: number;
  checked_segments: number;
  notes: string[];
}

export type TranslateTargetLanguage = "en" | "zh-Hans" | "zh-Hant";

export interface TranslateBatchFailure {
  segment_ids: number[];
  error: string;
}

/** Result of the `translate_segments` command (text.translate). */
export interface TranslateOutcome {
  provenance: string;
  target_language: TranslateTargetLanguage;
  requested: number;
  translated: number[];
  unchanged: number[];
  stale: number[];
  protected: number[];
  failed_batches: TranslateBatchFailure[];
  cancelled: boolean;
  notes: string[];
  segments: Segment[];
}

export type ProposalDecision =
  | { outcome: "applied"; segment: Segment }
  | { outcome: "rejected" }
  | { outcome: "stale"; segment: Segment | null }
  | { outcome: "not_pending"; status: RefineProposalStatus }
  | { outcome: "not_found" };

export type VoiceKind = "preset" | "design";

export interface VoiceProfile {
  id: string;
  name: string;
  kind: VoiceKind;
  provider_id: string;
  model_id: string;
  preset_voice: string;
  design_prompt: string;
  language: string;
  params_json: string;
  created_at: string;
  updated_at: string;
}

export interface VoiceProfileInput {
  id: string | null;
  name: string;
  kind: VoiceKind;
  provider_id: string;
  model_id: string;
  preset_voice: string | null;
  design_prompt: string | null;
  language: string;
  params_json: string | null;
}

export interface TtsAsset {
  key: string;
  path: string;
  sha256: string;
  duration_ms: number;
  sample_rate: number;
  channels: number;
  provider_id: string;
  model_id: string;
  voice_profile_id: string;
  created_at: string;
}

export interface ResolvedTtsAsset {
  segment_id: number;
  key: string;
  path: string;
  sha256: string;
  duration_ms: number;
  sample_rate: number;
  channels: number;
  provider_id: string;
  model_id: string;
  voice_profile_id: string;
  dub_skip: boolean;
}

export interface SynthesisOutcome {
  segment: Segment;
  asset: TtsAsset;
  path: string;
  cache_hit: boolean;
}

/** `dub.place` output (docs/19-dubbing-pipeline.md). */
export interface DubPlacement {
  segment_id: number;
  dub_start_ms: number;
  dub_end_ms: number;
  drift_ms: number;
  /** `drift_exceeded`, `skipped`, `no_clip`. */
  flags: string[];
}

export interface DubPlacementParams {
  anchor: "start" | "end";
  beat_ms: number;
  min_gap_ms: number;
  max_drift_ms: number;
}

export interface DubPlan {
  media_id: number;
  params: DubPlacementParams;
  placements: DubPlacement[];
  placed_count: number;
  drift_exceeded_count: number;
}

export type DubExportKind = "mix" | "dub_stem";

export interface DubMixReport {
  output_path: string;
  kind: DubExportKind;
  sample_rate: number;
  channels: number;
  original_frames: number;
  output_frames: number;
  limited_frames: number;
  clips: { segment_id: number; start_frame: number; frames: number; gain_db: number }[];
  /** Clips that used an `audio.spatialize` render instead of the raw TTS clip. */
  spatial_clips: number;
}

/** `audio.spatialize` render mode (default `source_trajectory`). */
export type SpatialMode = "source_trajectory" | "concurrent";

/** `spatialize_segment` result (mirrors `spatial::RenderOutcome`). */
export interface SpatialRenderOutcome {
  segment_id: number;
  media_id: number;
  mode: SpatialMode;
  render_key: string;
  spatial_track_key: string;
  tts_asset_key: string;
  /** Absolute path of the stereo render (use `convertFileSrc`). */
  path: string;
  cache_hit: boolean;
  analysis_cache_hit: boolean;
  report: unknown | null;
}
