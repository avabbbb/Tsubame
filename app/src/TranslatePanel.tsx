import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { canCancel, useJobs } from "./jobs";
import type {
  CapabilityTarget,
  MediaItem,
  Segment,
  TranslateOutcome,
  TranslateTargetLanguage,
} from "./types";
import { userMessage } from "./errors";

type Props = {
  media: MediaItem;
  segments: Segment[];
  onSegments: (segments: Segment[]) => void;
};

const LANGUAGES: { id: TranslateTargetLanguage; label: string }[] = [
  { id: "zh-Hans", label: "Chinese (Simplified)" },
  { id: "zh-Hant", label: "Chinese (Traditional)" },
  { id: "en", label: "English" },
];

const targetKey = (target: CapabilityTarget) =>
  `${target.provider_id}\u0000${target.model_id}`;

function summarize(outcome: TranslateOutcome) {
  const parts = [`Translated ${outcome.translated.length} of ${outcome.requested} line(s).`];
  if (outcome.protected.length)
    parts.push(`Kept ${outcome.protected.length} line(s) you edited yourself.`);
  if (outcome.stale.length)
    parts.push(`${outcome.stale.length} line(s) changed during the run and were left as is.`);
  const failed = outcome.failed_batches.reduce((sum, batch) => sum + batch.segment_ids.length, 0);
  if (failed) parts.push(`${failed} line(s) failed and can be translated again.`);
  if (outcome.cancelled) parts.push("Stopped before finishing.");
  return parts.join(" ");
}

export default function TranslatePanel({ media, segments, onSegments }: Props) {
  const [targets, setTargets] = useState<CapabilityTarget[]>([]);
  const [selectedKey, setSelectedKey] = useState("");
  const [language, setLanguage] = useState<TranslateTargetLanguage>("zh-Hans");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const { jobs, cancel } = useJobs({ media_id: media.id, kind: "translate" });
  const activeJob = jobs.find(canCancel) ?? null;

  const usable = useMemo(
    () =>
      targets.filter(
        (target) => target.available && target.provider_availability === "ready",
      ),
    [targets],
  );
  const selected = useMemo(
    () => usable.find((target) => targetKey(target) === selectedKey) ?? null,
    [selectedKey, usable],
  );

  const load = useCallback(async () => {
    const rows = await invoke<CapabilityTarget[]>("resolve_capability_targets", {
      capability: "text.translate",
    });
    setTargets(rows);
    setSelectedKey((current) => {
      const ready = rows.filter(
        (target) => target.available && target.provider_availability === "ready",
      );
      if (ready.some((target) => targetKey(target) === current)) return current;
      return ready[0] ? targetKey(ready[0]) : "";
    });
  }, []);

  useEffect(() => {
    setMessage("");
    void load().catch((error) => setMessage(userMessage(error)));
  }, [load, media.id]);

  const run = async () => {
    if (!selected) return;
    setBusy(true);
    setMessage("Translating…");
    try {
      const outcome = await invoke<TranslateOutcome>("translate_segments", {
        mediaId: media.id,
        segmentIds: null,
        providerId: selected.provider_id,
        modelId: selected.model_id,
        sourceLanguage: null,
        targetLanguage: language,
        overwriteHumanEdits: false,
      });
      onSegments(outcome.segments);
      setMessage(
        outcome.requested === 0
          ? "Every line already has an up-to-date translation."
          : summarize(outcome),
      );
    } catch (error) {
      setMessage(userMessage(error));
      // Batches applied before a cancel or failure are kept; show them.
      try {
        onSegments(await invoke<Segment[]>("get_segments", { mediaId: media.id }));
      } catch {
        // Keep the current list if the reload fails.
      }
    } finally {
      setBusy(false);
    }
  };

  const stop = async () => {
    if (!activeJob) return;
    try {
      await cancel(activeJob.id);
      setMessage("Stopping after the current batch…");
    } catch (error) {
      setMessage(userMessage(error));
    }
  };

  return (
    <section className="refine-panel">
      <div className="refine-head">
        <div>
          <p className="section-label">TRANSLATE</p>
          <strong>Translate for dubbing</strong>
          <small>Lines you translated yourself are never replaced.</small>
        </div>
      </div>

      {targets.length === 0 ? (
        <div className="refine-empty">
          Add a text model that can translate in Settings → Providers. A local server such
          as LM Studio works the same as a paid API.
        </div>
      ) : (
        <div className="refine-run">
          <select
            value={language}
            onChange={(event) => setLanguage(event.target.value as TranslateTargetLanguage)}
            aria-label="Target language"
          >
            {LANGUAGES.map((option) => (
              <option key={option.id} value={option.id}>
                {option.label}
              </option>
            ))}
          </select>
          <select
            value={selectedKey}
            onChange={(event) => setSelectedKey(event.target.value)}
            aria-label="Translation model"
          >
            {!usable.length && <option value="">No model is ready</option>}
            {usable.map((target) => (
              <option key={targetKey(target)} value={targetKey(target)}>
                {target.provider_name} · {target.display_name || target.model_id}
              </option>
            ))}
          </select>
          <button
            className="primary"
            disabled={busy || !selected || segments.length === 0}
            onClick={() => void run()}
          >
            {busy
              ? activeJob && activeJob.progress_total > 0
                ? `Translating ${activeJob.progress_done}/${activeJob.progress_total}…`
                : "Translating…"
              : "Translate"}
          </button>
          {activeJob && <button onClick={() => void stop()}>Cancel</button>}
        </div>
      )}

      {message && <div className="refine-message">{message}</div>}
    </section>
  );
}
