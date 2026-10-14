import { useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { canCancel, jobProgress, useJobs } from "./jobs";
import type { ResolvedTtsAsset, Segment, SpatialRenderOutcome } from "./types";

type Props = {
  segment: Segment;
  /** True while the Inspector holds unsaved edits for this sentence. */
  unsaved: boolean;
};

/**
 * Inspector section for the active clip: its format and an explicit
 * re-spatialize. Synthesis lives in the single Synthesize/Regenerate action and
 * listening in the takes list / context preview (SegmentRegeneratePanel).
 */
export default function SegmentDubPanel({ segment, unsaved }: Props) {
  const [asset, setAsset] = useState<ResolvedTtsAsset | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [spatialBusy, setSpatialBusy] = useState(false);
  const [render, setRender] = useState<SpatialRenderOutcome | null>(null);
  const { jobs: spatialJobs, cancel } = useJobs({ segment_id: segment.id, kind: "spatial" });
  const spatialJob = spatialJobs.find(canCancel) ?? null;
  const spatialFraction = spatialJob ? jobProgress(spatialJob) : null;

  useEffect(() => {
    setNote(null);
    void invoke<ResolvedTtsAsset | null>("get_tts_asset", { segmentId: segment.id })
      .then(setAsset)
      .catch((error) => {
        setAsset(null);
        setNote(String(error));
      });
  }, [segment.id, segment.revision, segment.tts_asset_key]);

  // A render belongs to one TTS clip; forget it when the clip changes.
  useEffect(() => {
    setRender((current) => (current && current.tts_asset_key === asset?.key ? current : null));
  }, [segment.id, asset?.key]);

  async function spatialize() {
    setSpatialBusy(true);
    setNote(null);
    try {
      const outcome = await invoke<SpatialRenderOutcome>("spatialize_segment", {
        segmentId: segment.id,
      });
      setRender(outcome);
      setNote(
        outcome.cache_hit
          ? "Reused a cached spatial render. Play in context to hear it."
          : "Spatialized. Play in context to hear it.",
      );
    } catch (error) {
      setNote(String(error));
    } finally {
      setSpatialBusy(false);
    }
  }

  async function stopSpatial() {
    if (!spatialJob) return;
    try {
      await cancel(spatialJob.id);
    } catch (error) {
      setNote(String(error));
    }
  }

  if (!asset && !note) return null;

  return (
    <div className="segment-state">
      <p className="section-label">SPATIAL AUDIO</p>
      {asset && (
        <>
          <p className="inspector-help">
            Active clip: {(asset.duration_ms / 1000).toFixed(2)} s · {asset.sample_rate / 1000} kHz ·{" "}
            {asset.channels === 1 ? "mono" : `${asset.channels} ch`}
            {segment.dirty_tts ? " · out of date" : ""}
          </p>
          <button
            className="secondary wide"
            disabled={spatialBusy || unsaved || segment.dirty_tts}
            title={
              unsaved
                ? "Save changes first"
                : segment.dirty_tts
                  ? "Regenerate the current translation first"
                  : "Follow the original sentence's position in the stereo field"
            }
            onClick={() => void spatialize()}
          >
            {spatialBusy
              ? spatialFraction !== null
                ? `Spatializing ${Math.round(spatialFraction * 100)}%…`
                : "Spatializing…"
              : render
                ? "Spatialize again"
                : "Spatialize"}
          </button>
          {spatialJob && (
            <button className="secondary wide" onClick={() => void stopSpatial()}>
              Cancel
            </button>
          )}
        </>
      )}
      {note && <p className="inspector-help">{note}</p>}
    </div>
  );
}
