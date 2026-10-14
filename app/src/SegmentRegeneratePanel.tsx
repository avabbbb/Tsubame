import { useCallback, useEffect, useImperativeHandle, useRef, useState } from "react";
import type { RefObject } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { canCancel, useJobs } from "./jobs";
import {
  REGENERATE_JOB_KIND,
  REGENERATE_SHORTCUT,
  REGENERATE_STAGES,
  STAGE_LABELS,
  acknowledgePushed,
  claimPlayback,
  isRevisionConflict,
  listTakes,
  playExclusive,
  previewSegmentContext,
  pushedLabel,
  regenerateBlocker,
  regenerateSegment,
  setActiveTake,
  stopPlayback,
  visibleTakes,
} from "./regenerate";
import { VOICE_KEY } from "./SegmentDubPanel";
import type { RegenerateOutcome, Segment, SpatialStageReport, TakeInfo } from "./types";

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

function spatialNote(report: SpatialStageReport): string | null {
  if (report.status === "skipped") {
    return report.reason?.startsWith("spatial_worker_unavailable")
      ? "Spatial audio skipped: the spatial worker is not available, so the raw voice clip is used."
      : `Spatial audio skipped${report.reason ? `: ${report.reason}` : "."}`;
  }
  if (report.status === "failed") {
    const first = report.errors[0];
    return `Spatial audio could not be rendered${first ? ` (${first.code})` : ""}; the raw voice clip is used.`;
  }
  return null;
}

function takeTitle(take: TakeInfo) {
  return `Take ${take.take_index + 1}`;
}

/**
 * Inspector section for the single-sentence regenerate loop: regenerate with
 * stage progress, context preview, takes (A/B, revert) and the pushed mark.
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
  const { jobs, cancel } = useJobs({ segment_id: segment.id, kind: REGENERATE_JOB_KIND });
  const job = jobs.find(canCancel) ?? null;
  const busy = running || job !== null;
  const blocker = regenerateBlocker(segment);

  const reloadTakes = useCallback(() => {
    void listTakes(segment.id)
      .then((rows) => setTakes(visibleTakes(rows)))
      .catch((err) => setError(String(err)));
  }, [segment.id]);

  useEffect(() => {
    reloadTakes();
  }, [reloadTakes, segment.revision, segment.tts_asset_key]);

  useEffect(() => {
    // A new selection starts clean and silences the previous sentence.
    setOutcome(null);
    setNote(null);
    setError(null);
    setCompareKey(null);
    setPlaying(null);
    stopPlayback();
  }, [segment.id]);

  const active = takes.find((take) => take.active) ?? null;
  const compare =
    takes.find((take) => take.tts_asset_key === compareKey && !take.active) ??
    takes.find((take) => !take.active && take.available) ??
    null;

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
    try {
      const preview = await previewSegmentContext(target.id, solo);
      await playSrc(preview.path, { kind: "context", solo });
      // Previewing clears the pushed mark (docs/20 § Preview).
      if (target.dub_pushed_ms > 0) onUpdated({ ...target, dub_pushed_ms: 0 });
    } catch (err) {
      setError(String(err));
    } finally {
      setPreviewBusy(false);
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
    const reason = regenerateBlocker(base);
    if (reason) {
      setError(reason);
      return;
    }
    setRunning(true);
    stop();
    try {
      // Without a clip yet, use the voice chosen above; otherwise the backend
      // keeps the active take's voice.
      const voice = base.tts_asset_key ? undefined : (localStorage.getItem(VOICE_KEY) ?? undefined);
      const result = await regenerateSegment(base, voice);
      setOutcome(result);
      onUpdated(result.segment);
      const moved = result.plan_change.moved.map((row) => row.segment_id);
      if (moved.length) onSegmentsChanged(moved);
      setCompareKey(result.previous_tts_asset_key);
      reloadTakes();
      await playContext(dubOnly, result.segment);
    } catch (err) {
      const message = String(err);
      if (isRevisionConflict(err)) {
        await reloadLatest("This sentence changed after you opened it. Reloaded the latest version; regenerate again.");
      } else if (message.includes("cancel")) {
        setNote("Regenerate cancelled.");
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
  const spatial = outcome ? spatialNote(outcome.spatial) : null;
  const pushedCount = outcome?.plan_change.pushed.length ?? 0;

  return (
    <div className="segment-state regenerate-panel">
      <p className="section-label">REGENERATE</p>

      {segment.dub_pushed_ms > 0 && (
        <div className="pushed-notice">
          <span className="pushed-dot" aria-hidden />
          <span>Dub {pushedLabel(segment.dub_pushed_ms)} by an earlier sentence.</span>
          <button onClick={() => void acknowledge()}>OK</button>
        </div>
      )}

      <button
        className="secondary wide regenerate-button"
        disabled={busy || blocker !== null}
        title={blocker ?? `Synthesize a new take, then re-place later sentences (${REGENERATE_SHORTCUT})`}
        onClick={() => void run()}
      >
        <span>{busy ? "Regenerating…" : unsaved ? "Save & Regenerate" : "Regenerate Segment"}</span>
        <kbd>{REGENERATE_SHORTCUT}</kbd>
      </button>
      {blocker && <p className="inspector-help">{blocker}</p>}

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

      <div className="preview-controls">
        <button
          className="secondary"
          disabled={previewBusy || !segment.tts_asset_key || segment.dub_skip}
          onClick={() => void playContext()}
        >
          {previewBusy ? "Rendering…" : dubOnly ? "▶ Play dub" : "▶ Play in context"}
        </button>
        <label className="dub-only-toggle">
          <input
            type="checkbox"
            checked={dubOnly}
            onChange={(event) => setDubOnly(event.target.checked)}
          />
          Dub only
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

      {note && <p className="inspector-help">{note}</p>}
      {error && (
        <div className="editor-alert">
          <strong>Regenerate</strong>
          <span>{error}</span>
        </div>
      )}
    </div>
  );
}
