import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { save } from "@tauri-apps/plugin-dialog";
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
        {busy ? "Exporting…" : "Export dubbed audio"}
      </button>
      {message && <span className="subtitle-message">{message}</span>}
    </div>
  );
}
