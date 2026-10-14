import { useEffect, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { canCancel, jobProgress, useJobs } from "./jobs";
import { claimPlayback } from "./regenerate";
import type {
  ResolvedTtsAsset,
  Segment,
  SpatialRenderOutcome,
  SynthesisOutcome,
  VoiceProfile,
} from "./types";

type Props = {
  segment: Segment;
  /** True while the Inspector holds unsaved edits for this sentence. */
  unsaved: boolean;
  onUpdated: (segment: Segment) => void;
};

export const VOICE_KEY = "tsubame.dub.voiceProfileId";

/** Inspector section: synthesize one sentence and preview the clip alone. */
export default function SegmentDubPanel({ segment, unsaved, onUpdated }: Props) {
  const [profiles, setProfiles] = useState<VoiceProfile[]>([]);
  const [voiceId, setVoiceId] = useState<string>(
    () => localStorage.getItem(VOICE_KEY) ?? "",
  );
  const [asset, setAsset] = useState<ResolvedTtsAsset | null>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);
  const [spatialBusy, setSpatialBusy] = useState(false);
  const [render, setRender] = useState<SpatialRenderOutcome | null>(null);
  const { jobs: spatialJobs, cancel } = useJobs({ segment_id: segment.id, kind: "spatial" });
  const spatialJob = spatialJobs.find(canCancel) ?? null;
  const spatialFraction = spatialJob ? jobProgress(spatialJob) : null;

  useEffect(() => {
    void invoke<VoiceProfile[]>("list_voice_profiles")
      .then((rows) => {
        setProfiles(rows);
        setVoiceId((current) =>
          rows.some((row) => row.id === current) ? current : (rows[0]?.id ?? ""),
        );
      })
      .catch((error) => setNote(String(error)));
  }, []);

  useEffect(() => {
    setNote(null);
    void invoke<ResolvedTtsAsset | null>("get_tts_asset", { segmentId: segment.id })
      .then(setAsset)
      .catch((error) => {
        setAsset(null);
        setNote(String(error));
      });
  }, [segment.id, segment.revision]);

  // A render belongs to one TTS clip; drop the preview when the clip changes.
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
      setNote(outcome.cache_hit ? "Reused a cached spatial render." : "Spatialized.");
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

  function chooseVoice(id: string) {
    setVoiceId(id);
    localStorage.setItem(VOICE_KEY, id);
  }

  async function synthesize() {
    if (!voiceId) return;
    setBusy(true);
    setNote(null);
    try {
      const outcome = await invoke<SynthesisOutcome>("synthesize_segment", {
        segmentId: segment.id,
        expectedRevision: segment.revision,
        voiceProfileId: voiceId,
      });
      onUpdated(outcome.segment);
      setNote(outcome.cache_hit ? "Reused a cached clip." : "Synthesized.");
    } catch (error) {
      setNote(String(error));
    } finally {
      setBusy(false);
    }
  }

  const hasTranslation = segment.translated_text.trim().length > 0;
  const blocked = !voiceId || !hasTranslation || unsaved || busy;

  return (
    <div className="segment-state">
      <p className="section-label">DUB VOICE</p>
      {profiles.length ? (
        <label>
          Voice
          <select value={voiceId} onChange={(event) => chooseVoice(event.target.value)}>
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

      <button
        className="secondary wide"
        disabled={blocked}
        title={
          unsaved
            ? "Save changes first"
            : !hasTranslation
              ? "This sentence has no translation yet"
              : undefined
        }
        onClick={() => void synthesize()}
      >
        {busy ? "Synthesizing…" : asset ? "Synthesize again" : "Synthesize"}
      </button>

      {asset && (
        <>
          <audio
            key={asset.key}
            controls
            preload="none"
            src={convertFileSrc(asset.path)}
            onPlay={(event) => claimPlayback(event.currentTarget)}
            style={{ width: "100%" }}
          />
          <p className="inspector-help">
            {(asset.duration_ms / 1000).toFixed(2)} s · {asset.sample_rate / 1000} kHz ·{" "}
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
                  ? "Synthesize the current translation first"
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
          {render && (
            <audio
              key={render.render_key}
              controls
              preload="none"
              src={convertFileSrc(render.path)}
              onPlay={(event) => claimPlayback(event.currentTarget)}
              style={{ width: "100%" }}
            />
          )}
        </>
      )}
      {note && <p className="inspector-help">{note}</p>}
    </div>
  );
}
