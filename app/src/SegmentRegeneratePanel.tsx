import { useCallback, useEffect, useImperativeHandle, useRef, useState } from "react";
import type { RefObject } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { canCancel, jobProgress, useJobs } from "./jobs";
import {
  REGENERATE_JOB_KIND,
  REGENERATE_SHORTCUT,
  REGENERATE_STAGES,
  STAGE_LABELS,
  VOICE_KEY,
  acknowledgePushed,
  claimPlayback,
  confirmTranslation,
  isRevisionConflict,
  isSpatialPending,
  isSuperseded,
  loadMixSettings,
  onMixSettingsChange,
  listTakes,
  playExclusive,
  previewSegmentContext,
  pushedLabel,
  regenerateBlocker,
  regenerateSegment,
  regenerateVoice,
  setActiveTake,
  stopPlayback,
  visibleTakes,
} from "./regenerate";
import type {
  RegenerateOutcome,
  Segment,
  SpatialStageReport,
  TakeInfo,
  VoiceProfile,
} from "./types";

/** What the Inspector's Ctrl/Cmd+Enter binding calls. */
export interface RegenerateControl {
  run: () => void;
}

type Props = {
  segment: Segment;
  /** True while the Inspector holds unsaved edits for this sentence. */
  unsaved: boolean;
  /** Saves pending edits; resolves to the saved Segment or null on failure. */
  saveFirst: () => Promise<Segment | null>;
  onUpdated: (segment: Segment) => void;
  /** Re-reads other Segments whose placement/pushed flag changed. */
  onSegmentsChanged: (ids: number[]) => void;
  controlRef: RefObject<RegenerateControl | null>;
};

type Playing =
  | { kind: "context"; solo: boolean }
  | { kind: "take"; key: string }
  | null;

/** Says what a skipped/failed spatial stage means for listening. */
function spatialNote(report: SpatialStageReport, spatialFollow: boolean): string | null {
  const then = spatialFollow
    ? " Play in context needs it: re-spatialize (⋯ below), use Dub only, or turn Spatial follow off in the export bar."
    : "";
  if (report.status === "skipped") {
    return report.reason?.startsWith("spatial_worker_unavailable")
      ? `Spatial audio skipped: the spatial worker is not available.${then}`
      : `Spatial audio skipped${report.reason ? `: ${report.reason}` : "."}${then}`;
  }
  if (report.status === "failed") {
    const first = report.errors[0];
    return `Spatial audio could not be rendered${first ? ` (${first.code})` : ""}.${then}`;
  }
  return null;
}

const SPATIAL_PENDING_NOTE =
  "The spatial audio for this sentence is not ready, so Play in context would not sound like the export. Wait for it, re-spatialize (⋯ below), or play Dub only.";

function takeTitle(take: TakeInfo) {
  return `Take ${take.take_index + 1}`;
}

/**
 * Inspector section for the single-sentence dub loop: the Dub voice selector,
 * the one Synthesize/Regenerate action with stage progress, context preview,
 * takes (A/B, revert) and the pushed mark.
 */
export default function SegmentRegeneratePanel({
  segment,
  unsaved,
  saveFirst,
  onUpdated,
  onSegmentsChanged,
  controlRef,
}: Props) {
  const audioRef = useRef<HTMLAudioElement | null>(null);
  const [takes, setTakes] = useState<TakeInfo[]>([]);
  const [running, setRunning] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [outcome, setOutcome] = useState<RegenerateOutcome | null>(null);
  const [dubOnly, setDubOnly] = useState(false);
  const [previewBusy, setPreviewBusy] = useState(false);
  const [playing, setPlaying] = useState<Playing>(null);
  const [compareKey, setCompareKey] = useState<string | null>(null);
  const [switching, setSwitching] = useState<string | null>(null);
  const [profiles, setProfiles] = useState<VoiceProfile[]>([]);
  /** The selector's last choice; the voice for sentences without a clip. */
  const [preferredVoice, setPreferredVoice] = useState<string>(
    () => localStorage.getItem(VOICE_KEY) ?? "",
  );
  /** A voice picked for this sentence that differs from its active take's. */
  const [voiceOverride, setVoiceOverride] = useState<string | null>(null);
  const { jobs, cancel } = useJobs({ segment_id: segment.id, kind: REGENERATE_JOB_KIND });
  const job = jobs.find(canCancel) ?? null;
  const busy = running || job !== null;
  // A re-spatialize of this sentence (⋯ menu) in progress.
  const { jobs: spatialJobs } = useJobs({ segment_id: segment.id, kind: "spatial" });
  const spatialJob = spatialJobs.find(canCancel) ?? null;
  const spatialFraction = spatialJob ? jobProgress(spatialJob) : null;
  const [spatialFollow, setSpatialFollow] = useState(() => loadMixSettings().spatial);
  useEffect(() => onMixSettingsChange(() => setSpatialFollow(loadMixSettings().spatial)), []);
  /** The mix preview refused (`spatial_pending`); cleared on the next attempt. */
  const [spatialPending, setSpatialPending] = useState(false);
  const [confirming, setConfirming] = useState(false);

  const reloadTakes = useCallback(() => {
    void listTakes(segment.id)
      .then((rows) => setTakes(visibleTakes(rows)))
      .catch((err) => setError(String(err)));
  }, [segment.id]);

  useEffect(() => {
    reloadTakes();
  }, [reloadTakes, segment.revision, segment.tts_asset_key]);

  useEffect(() => {
    void invoke<VoiceProfile[]>("list_voice_profiles")
      .then((rows) => {
        setProfiles(rows);
        setPreferredVoice((current) =>
          rows.some((row) => row.id === current) ? current : (rows[0]?.id ?? ""),
        );
      })
      .catch((err) => setError(String(err)));
  }, []);

  useEffect(() => {
    // A new selection starts clean and silences the previous sentence.
    setOutcome(null);
    setNote(null);
    setError(null);
    setCompareKey(null);
    setPlaying(null);
    setVoiceOverride(null);
    setSpatialPending(false);
    stopPlayback();
  }, [segment.id]);

  const active = takes.find((take) => take.active) ?? null;
  const compare =
    takes.find((take) => take.tts_asset_key === compareKey && !take.active) ??
    takes.find((take) => !take.active && take.available) ??
    null;

  const hasClip = segment.tts_asset_key !== null;
  const activeVoice = active?.voice_profile_id ?? "";
  const activeVoiceKnown = profiles.some((profile) => profile.id === activeVoice);
  // With a clip the selector shows the active take's voice until the user
  // picks another one for this sentence.
  const selectedVoice =
    voiceOverride ?? (hasClip && activeVoiceKnown ? activeVoice : preferredVoice);
  const voiceBlocker =
    !hasClip && !preferredVoice ? "Create a dub voice in Settings first." : null;
  const blocker = regenerateBlocker(segment) ?? voiceBlocker;
  const action = hasClip ? "Regenerate" : "Synthesize";

  function chooseVoice(id: string) {
    setPreferredVoice(id);
    localStorage.setItem(VOICE_KEY, id);
    setVoiceOverride(hasClip && !(activeVoiceKnown && id === activeVoice) ? id : null);
  }

  async function playSrc(path: string, next: Playing, offset = 0) {
    const audio = audioRef.current;
    if (!audio) return;
    audio.src = convertFileSrc(path);
    await playExclusive(audio);
    if (offset > 0) audio.currentTime = offset;
    setPlaying(next);
  }

  async function reloadLatest(message: string) {
    const latest = await invoke<Segment | null>("get_segment", { id: segment.id }).catch(
      () => null,
    );
    if (latest) onUpdated(latest);
    setError(latest ? message : "This sentence no longer exists.");
    reloadTakes();
  }

  async function playContext(solo = dubOnly, target: Segment = segment) {
    setPreviewBusy(true);
    setError(null);
    setSpatialPending(false);
    try {
      const preview = await previewSegmentContext(target.id, solo);
      await playSrc(preview.path, { kind: "context", solo });
      // Previewing clears the pushed mark (docs/20 § Preview).
      if (target.dub_pushed_ms > 0) onUpdated({ ...target, dub_pushed_ms: 0 });
    } catch (err) {
      // No silent raw-clip fallback: say why and offer Dub only.
      if (isSpatialPending(err)) setSpatialPending(true);
      else setError(String(err));
    } finally {
      setPreviewBusy(false);
    }
  }

  async function keepTranslation() {
    setConfirming(true);
    setError(null);
    try {
      onUpdated(await confirmTranslation(segment));
      setNote("Translation kept. Regenerate when you want a new take.");
    } catch (err) {
      if (isRevisionConflict(err)) {
        await reloadLatest("This sentence changed after you opened it. Reloaded the latest version.");
      } else {
        setError(String(err));
      }
    } finally {
      setConfirming(false);
    }
  }

  async function playTake(take: TakeInfo) {
    if (!take.path) return;
    if (!take.active) setCompareKey(take.tts_asset_key);
    setError(null);
    try {
      await playSrc(take.path, { kind: "take", key: take.tts_asset_key });
    } catch (err) {
      setError(String(err));
    }
  }

  /** A/B: switch between the active take and the compare take at the same position. */
  async function switchAB() {
    if (!active?.path || !compare?.path) return;
    const audio = audioRef.current;
    const position =
      playing?.kind === "take" && audio && !audio.paused ? audio.currentTime : 0;
    const next = playing?.kind === "take" && playing.key === active.tts_asset_key ? compare : active;
    if (!next.path) return;
    setError(null);
    try {
      await playSrc(next.path, { kind: "take", key: next.tts_asset_key }, position);
    } catch (err) {
      setError(String(err));
    }
  }

  function stop() {
    stopPlayback();
    setPlaying(null);
  }

  async function useTake(take: TakeInfo) {
    setSwitching(take.tts_asset_key);
    setError(null);
    setNote(null);
    try {
      const result = await setActiveTake(segment, take.tts_asset_key);
      onUpdated(result.segment);
      const moved = result.plan_change.moved.map((row) => row.segment_id);
      if (moved.length) onSegmentsChanged(moved);
      const pushed = result.plan_change.pushed.length;
      setNote(
        `${takeTitle(take)} is active.${pushed ? ` ${pushed} later sentence${pushed === 1 ? "" : "s"} pushed.` : ""}`,
      );
      reloadTakes();
    } catch (err) {
      if (isRevisionConflict(err)) {
        await reloadLatest("This sentence changed after you opened it. Reloaded the latest version; choose the take again.");
      } else {
        setError(String(err));
      }
    } finally {
      setSwitching(null);
    }
  }

  async function acknowledge() {
    try {
      const rows = await acknowledgePushed([segment.id]);
      rows.forEach(onUpdated);
    } catch (err) {
      setError(String(err));
    }
  }

  async function run() {
    if (busy) return;
    setError(null);
    setNote(null);
    let base: Segment | null = segment;
    if (unsaved) {
      base = await saveFirst();
      if (!base) return;
    }
    const baseHasClip = base.tts_asset_key !== null;
    const reason =
      regenerateBlocker(base) ??
      (!baseHasClip && !preferredVoice ? "Create a dub voice in Settings first." : null);
    if (reason) {
      setError(reason);
      return;
    }
    setRunning(true);
    stop();
    try {
      // docs/20 § Frontend: the selected voice without a clip; otherwise the
      // active take's voice unless the selector was changed for this sentence.
      const voice = regenerateVoice({
        hasClip: baseHasClip,
        selected: preferredVoice,
        override: voiceOverride,
        activeVoiceKnown,
      });
      const result = await regenerateSegment(base, voice);
      setVoiceOverride(null);
      setOutcome(result);
      onUpdated(result.segment);
      const moved = result.plan_change.moved.map((row) => row.segment_id);
      if (moved.length) onSegmentsChanged(moved);
      setCompareKey(result.previous_tts_asset_key);
      reloadTakes();
      if (result.duplicate_of_take !== null) {
        setNote(
          result.new_take
            ? null
            : `The voice returned the same audio as Take ${result.duplicate_of_take + 1}; no duplicate was kept.`,
        );
      }
      await playContext(dubOnly, result.segment);
    } catch (err) {
      const message = String(err);
      if (isRevisionConflict(err)) {
        await reloadLatest("This sentence changed after you opened it. Reloaded the latest version; regenerate again.");
      } else if (isSuperseded(err)) {
        // A newer regenerate or a take choice won; this result is not applied.
        setNote("A newer request replaced this one; its result was not applied.");
        reloadTakes();
      } else if (message.includes("cancel")) {
        setNote(`${baseHasClip ? "Regenerate" : "Synthesize"} cancelled.`);
      } else {
        setError(message);
      }
    } finally {
      setRunning(false);
    }
  }

  const runRef = useRef(run);
  runRef.current = run;
  useImperativeHandle(controlRef, () => ({ run: () => void runRef.current() }), []);

  const stageIndex = job ? REGENERATE_STAGES.indexOf(job.stage as (typeof REGENERATE_STAGES)[number]) : -1;
  const spatial = outcome ? spatialNote(outcome.spatial, spatialFollow) : null;
  const pushedCount = outcome?.plan_change.pushed.length ?? 0;

  return (
    <div className="segment-state regenerate-panel">
      <p className="section-label">DUB VOICE</p>
      {profiles.length ? (
        <label>
          Voice
          <select
            value={selectedVoice}
            disabled={busy}
            onChange={(event) => chooseVoice(event.target.value)}
          >
            {profiles.map((profile) => (
              <option key={profile.id} value={profile.id}>
                {profile.name} · {profile.kind}
              </option>
            ))}
          </select>
        </label>
      ) : (
        <p className="inspector-help">Create a dub voice in Settings first.</p>
      )}
      {voiceOverride && (
        <p className="inspector-help">The next take uses this voice instead of the active take's.</p>
      )}

      {segment.dub_pushed_ms > 0 && (
        <div className="pushed-notice">
          <span className="pushed-dot" aria-hidden />
          <span>Dub {pushedLabel(segment.dub_pushed_ms)} by an earlier sentence.</span>
          <button onClick={() => void acknowledge()}>OK</button>
        </div>
      )}

      <button
        className="primary wide regenerate-button"
        disabled={busy || blocker !== null}
        title={
          blocker ??
          (hasClip
            ? `Synthesize a new take, then re-place later sentences (${REGENERATE_SHORTCUT})`
            : `Synthesize this sentence, then place it and re-place later sentences (${REGENERATE_SHORTCUT})`)
        }
        onClick={() => void run()}
      >
        <span>
          {busy
            ? hasClip
              ? "Regenerating…"
              : "Synthesizing…"
            : `${unsaved ? "Save & " : ""}${action}`}
        </span>
        <kbd>{REGENERATE_SHORTCUT}</kbd>
      </button>
      {blocker && <p className="inspector-help">{blocker}</p>}
      {segment.dirty_translation && !segment.dub_skip && segment.translated_text.trim() !== "" && (
        <button
          className="secondary"
          disabled={busy || confirming || unsaved}
          title={
            unsaved
              ? "Save changes first"
              : "The translation still fits the edited original: keep it (no new take is made)"
          }
          onClick={() => void keepTranslation()}
        >
          {confirming ? "…" : "Keep translation"}
        </button>
      )}

      {busy && (
        <div className="regen-progress">
          <ol className="regen-stages">
            {REGENERATE_STAGES.map((stage, index) => (
              <li
                key={stage}
                className={index < stageIndex ? "done" : index === stageIndex ? "current" : ""}
              >
                {STAGE_LABELS[stage]}
              </li>
            ))}
          </ol>
          {job && (
            <button className="secondary" onClick={() => void cancel(job.id).catch((err) => setError(String(err)))}>
              Cancel
            </button>
          )}
        </div>
      )}

      {outcome && !busy && (
        <p className="inspector-help">
          {takeTitle({ take_index: outcome.take_index } as TakeInfo)}
          {outcome.cache_hit ? " (cached)" : ""}
          {pushedCount ? ` · ${pushedCount} later sentence${pushedCount === 1 ? "" : "s"} pushed` : ""}
        </p>
      )}
      {spatial && <p className="inspector-help subtle-note">{spatial}</p>}

      {spatialJob && (
        <p className="inspector-help" role="status">
          {spatialFraction !== null
            ? `Rendering spatial audio ${Math.round(spatialFraction * 100)}%…`
            : "Rendering spatial audio…"}{" "}
          Play in context waits for it; Dub only plays the voice now.
        </p>
      )}
      <div className="preview-controls">
        <button
          className="secondary"
          disabled={
            previewBusy ||
            !segment.tts_asset_key ||
            segment.dub_skip ||
            (!dubOnly && spatialFollow && (spatialJob !== null || busy))
          }
          title={
            dubOnly
              ? "Play this sentence's voice clip alone (no spatial audio, no original)"
              : `Play from 1 s before the sentence to 1 s after its dub, mixed exactly like the export${spatialFollow ? " (Spatial follow on)" : ""}`
          }
          onClick={() => void playContext()}
        >
          {previewBusy ? "Rendering…" : dubOnly ? "▶ Play dub only" : "▶ Play in context"}
        </button>
        <label className="dub-only-toggle">
          <input
            type="checkbox"
            checked={dubOnly}
            onChange={(event) => setDubOnly(event.target.checked)}
          />
          Dub only (raw voice)
        </label>
        {playing && (
          <button className="secondary" onClick={stop}>
            ■ Stop
          </button>
        )}
      </div>
      <audio
        ref={audioRef}
        preload="none"
        onPlay={(event) => claimPlayback(event.currentTarget)}
        onEnded={() => setPlaying(null)}
        hidden
      />

      {takes.length > 0 && (
        <div className="takes-list">
          <div className="takes-header">
            <p className="section-label">TAKES</p>
            {active?.path && compare?.path && (
              <button
                className="secondary ab-button"
                title={`Switch between ${takeTitle(active)} (A) and ${takeTitle(compare)} (B) at the same position`}
                onClick={() => void switchAB()}
              >
                A/B · {takeTitle(compare)}
              </button>
            )}
          </div>
          {takes.map((take) => {
            const isPlaying = playing?.kind === "take" && playing.key === take.tts_asset_key;
            return (
              <div
                key={take.tts_asset_key}
                className={["take-row", take.active ? "active" : "", isPlaying ? "playing" : ""]
                  .filter(Boolean)
                  .join(" ")}
              >
                <button
                  className="take-play"
                  disabled={!take.available || !take.path}
                  title={take.available ? "Play this take alone" : "The clip file is missing"}
                  onClick={() => (isPlaying ? stop() : void playTake(take))}
                >
                  {isPlaying ? "■" : "▶"}
                </button>
                <span className="take-meta">
                  <strong>
                    {takeTitle(take)}
                    {take.active && <span className="state-badge clean">Active</span>}
                    {!take.active && compare?.tts_asset_key === take.tts_asset_key && (
                      <span className="state-badge">B</span>
                    )}
                  </strong>
                  <small>
                    {take.duration_ms !== null ? `${(take.duration_ms / 1000).toFixed(2)} s` : "—"}
                    {take.available ? "" : " · missing"}
                  </small>
                </span>
                {!take.active && (
                  <button
                    className="secondary"
                    disabled={!take.available || unsaved || busy || switching !== null}
                    title={unsaved ? "Save changes first" : undefined}
                    onClick={() => void useTake(take)}
                  >
                    {switching === take.tts_asset_key ? "…" : "Use this take"}
                  </button>
                )}
              </div>
            );
          })}
        </div>
      )}

      {spatialPending && !dubOnly && (
        <div className="editor-alert" role="status">
          <span>{SPATIAL_PENDING_NOTE}</span>
          <button
            className="secondary"
            onClick={() => {
              setDubOnly(true);
              void playContext(true);
            }}
          >
            ▶ Play dub only
          </button>
        </div>
      )}
      {note && <p className="inspector-help">{note}</p>}
      {error && (
        <div className="editor-alert">
          <strong>{action}</strong>
          <span>{error}</span>
        </div>
      )}
    </div>
  );
}
