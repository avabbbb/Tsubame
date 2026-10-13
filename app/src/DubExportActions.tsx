import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
import { canCancel, jobProgress, useJobs } from "./jobs";
import type { DubExportKind, DubMixReport, DubPlan, MediaItem } from "./types";

type Props = {
  media: MediaItem;
  plan: DubPlan | null;
  onRefreshPlan: () => void;
};

/**
 * Minimal Studio entry for `audio.mix`: renders original + placed dub clips
 * (or the dub-only stem) to WAV. The original file is never modified.
 */
export default function DubExportActions({ media, plan, onRefreshPlan }: Props) {
  const [kind, setKind] = useState<DubExportKind>("mix");
  const [limiter, setLimiter] = useState(true);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const { jobs, cancel } = useJobs({ media_id: media.id, kind: "mix" });
  const activeJob = jobs.find(canCancel) ?? null;
  const fraction = activeJob ? jobProgress(activeJob) : null;

  async function exportAudio() {
    const base = media.title.replace(/[\\/:*?"<>|]/g, "_");
    const suffix = kind === "mix" ? "dubbed" : "dub-stem";
    let path = await save({
      defaultPath: `${base}.${suffix}.wav`,
      filters: [{ name: "WAV audio", extensions: ["wav"] }],
    });
    if (!path) return;
    if (!path.toLowerCase().endsWith(".wav")) path += ".wav";

    setBusy(true);
    setMessage(null);
    try {
      const report = await invoke<DubMixReport>("export_dub_mix", {
        mediaId: media.id,
        outputPath: path,
        options: { kind, limiter },
      });
      const limited =
        report.limited_frames > 0 ? `, limiter touched ${report.limited_frames} frames` : "";
      setMessage(`Exported ${report.clips.length} dub clips${limited}.`);
      onRefreshPlan();
    } catch (errorValue) {
      setMessage(String(errorValue));
    } finally {
      setBusy(false);
    }
  }

  async function stop() {
    if (!activeJob) return;
    try {
      await cancel(activeJob.id);
    } catch (errorValue) {
      setMessage(String(errorValue));
    }
  }

  const placed = plan?.placed_count ?? 0;
  const drift = plan?.drift_exceeded_count ?? 0;

  return (
    <div className="subtitle-actions dub-export-actions">
      <span className="section-label">DUB</span>
      <small className="dub-plan-summary">
        {plan
          ? `${placed} placed${drift ? ` · ${drift} over drift limit` : ""}`
          : "No dub plan yet"}
      </small>
      <select
        value={kind}
        onChange={(event) => setKind(event.target.value as DubExportKind)}
        title="Export"
      >
        <option value="mix">Original + dub</option>
        <option value="dub_stem">Dub stem only</option>
      </select>
      <label className="dub-limiter-toggle" title="Peak limiter on the sum bus only">
        <input
          type="checkbox"
          checked={limiter}
          disabled={kind === "dub_stem"}
          onChange={(event) => setLimiter(event.target.checked)}
        />
        Limiter
      </label>
      <button disabled={busy || placed === 0} onClick={() => void exportAudio()}>
        {busy
          ? fraction !== null
            ? `Exporting ${Math.round(fraction * 100)}%…`
            : "Exporting…"
          : "Export dubbed audio"}
      </button>
      {activeJob && <button onClick={() => void stop()}>Cancel</button>}
      {message && <span className="subtitle-message">{message}</span>}
    </div>
  );
}
