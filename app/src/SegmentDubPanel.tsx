import { useEffect, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import type {
  ResolvedTtsAsset,
  Segment,
  SynthesisOutcome,
  VoiceProfile,
} from "./types";

type Props = {
  segment: Segment;
  /** True while the Inspector holds unsaved edits for this sentence. */
  unsaved: boolean;
  onUpdated: (segment: Segment) => void;
};

const VOICE_KEY = "tsubame.dub.voiceProfileId";

/** Inspector section: synthesize one sentence and preview the clip alone. */
export default function SegmentDubPanel({ segment, unsaved, onUpdated }: Props) {
  const [profiles, setProfiles] = useState<VoiceProfile[]>([]);
  const [voiceId, setVoiceId] = useState<string>(
    () => localStorage.getItem(VOICE_KEY) ?? "",
  );
  const [asset, setAsset] = useState<ResolvedTtsAsset | null>(null);
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);

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
            style={{ width: "100%" }}
          />
          <p className="inspector-help">
            {(asset.duration_ms / 1000).toFixed(2)} s · {asset.sample_rate / 1000} kHz ·{" "}
            {asset.channels === 1 ? "mono" : `${asset.channels} ch`}
            {segment.dirty_tts ? " · out of date" : ""}
          </p>
        </>
      )}
      {note && <p className="inspector-help">{note}</p>}
    </div>
  );
}
