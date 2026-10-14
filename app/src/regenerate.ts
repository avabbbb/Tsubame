// Single-sentence regenerate loop (docs/20-segment-regenerate.md): command
// wrappers, the exclusive preview player and small formatting helpers.
import { invoke } from "@tauri-apps/api/core";
import type {
  ActiveTakeOutcome,
  RegenerateOutcome,
  Segment,
  SegmentPreview,
  TakeInfo,
} from "./types";

export const REGENERATE_JOB_KIND = "regenerate";
/** Job stages in order (`job.stage`); `done` follows the last one. */
export const REGENERATE_STAGES = ["tts", "spatialize", "replan"] as const;
export const STAGE_LABELS: Record<(typeof REGENERATE_STAGES)[number], string> = {
  tts: "Voice",
  spatialize: "Spatialize",
  replan: "Re-place",
};
/** Default context pre-roll (docs/20 § Preview). */
export const PREVIEW_PRE_ROLL_MS = 1000;
/** The Inspector shows at most this many takes (backend keeps 5). */
export const MAX_TAKES_SHOWN = 5;

export const isMac =
  typeof navigator !== "undefined" && /Mac|iPhone|iPad/.test(navigator.platform);
export const REGENERATE_SHORTCUT = isMac ? "⌘↵" : "Ctrl+↵";

/** `Ctrl+Enter` (or `Cmd+Enter` on macOS). */
export const isRegenerateShortcut = (event: KeyboardEvent) =>
  event.key === "Enter" && (event.metaKey || event.ctrlKey) && !event.altKey && !event.shiftKey;

/** localStorage key of the Dub voice selector's last choice (new sentences use it). */
export const VOICE_KEY = "tsubame.dub.voiceProfileId";

/**
 * The `voice_profile_id` the Inspector's single Synthesize/Regenerate action
 * sends (docs/20 § Frontend). Without a clip yet: the selected voice. With an
 * active take: undefined, so the backend keeps the take's voice, unless the
 * user changed the selector for this sentence (`override`) or the take's voice
 * is unknown / no longer exists (`activeVoiceKnown` false).
 */
export function regenerateVoice(options: {
  hasClip: boolean;
  selected: string;
  override: string | null;
  activeVoiceKnown: boolean;
}): string | undefined {
  const { hasClip, selected, override, activeVoiceKnown } = options;
  if (!hasClip) return selected || undefined;
  if (override) return override;
  return activeVoiceKnown ? undefined : selected || undefined;
}

export const regenerateSegment = (segment: Segment, voiceProfileId?: string) =>
  invoke<RegenerateOutcome>("regenerate_segment", {
    segmentId: segment.id,
    expectedRevision: segment.revision,
    voiceProfileId: voiceProfileId ?? null,
  });

export const listTakes = (segmentId: number) => invoke<TakeInfo[]>("list_takes", { segmentId });

export const setActiveTake = (segment: Segment, takeKey: string) =>
  invoke<ActiveTakeOutcome>("set_active_take", {
    segmentId: segment.id,
    takeKey,
    expectedRevision: segment.revision,
  });

export const acknowledgePushed = (segmentIds: number[]) =>
  invoke<Segment[]>("acknowledge_pushed", { segmentIds });

export const previewSegmentContext = (segmentId: number, solo: boolean) =>
  invoke<SegmentPreview>("preview_segment_context", {
    segmentId,
    preRollMs: PREVIEW_PRE_ROLL_MS,
    solo,
  });

export const isRevisionConflict = (error: unknown) =>
  String(error).includes("revision conflict");

/**
 * Why regenerate cannot run for this sentence right now, or null. A stale
 * translation is never re-translated automatically (docs/20 § Product rules).
 */
export function regenerateBlocker(segment: Segment): string | null {
  if (segment.dub_skip) return "This sentence is excluded from the dub.";
  if (segment.dirty_translation)
    return "The translation is out of date because the original changed. Re-translate or edit the translation first.";
  if (!segment.translated_text.trim()) return "This sentence has no translation yet.";
  return null;
}

/** "pushed later by 1.2s" */
export const pushedLabel = (ms: number) => `pushed later by ${(ms / 1000).toFixed(1)}s`;

export const isPushed = (segment: Pick<Segment, "dub_pushed_ms">) => segment.dub_pushed_ms > 0;

/** The newest takes first, at most `MAX_TAKES_SHOWN`. */
export const visibleTakes = (takes: TakeInfo[]) =>
  [...takes].sort((a, b) => b.take_index - a.take_index).slice(0, MAX_TAKES_SHOWN);

// ----- exclusive playback -----

let playing: HTMLAudioElement | null = null;

/**
 * Plays `audio` and stops whatever preview was playing before, so only one
 * preview (context, take, clip) sounds at a time.
 */
export async function playExclusive(audio: HTMLAudioElement) {
  claimPlayback(audio);
  audio.currentTime = 0;
  await audio.play();
}

/** `onPlay` handler for native `<audio controls>`: pauses the previous one. */
export function claimPlayback(audio: HTMLAudioElement) {
  if (playing && playing !== audio) playing.pause();
  playing = audio;
}

export function stopPlayback() {
  playing?.pause();
  playing = null;
}
