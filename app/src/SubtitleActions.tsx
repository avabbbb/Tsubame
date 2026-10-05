import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open, save } from "@tauri-apps/plugin-dialog";
import type { MediaItem, Segment, SubtitleTextMode } from "./types";

type Props = {
  media: MediaItem;
  segmentCount: number;
  onImported: (segments: Segment[]) => void;
};

export default function SubtitleActions({
  media,
  segmentCount,
  onImported,
}: Props) {
  const [textMode, setTextMode] = useState<SubtitleTextMode>("source");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);

  async function importSubtitle() {
    const path = await open({
      multiple: false,
      filters: [{ name: "Subtitles", extensions: ["srt", "vtt"] }],
    });
    if (!path || Array.isArray(path)) return;

    if (
      segmentCount > 0 &&
      !window.confirm(
        "Importing subtitles replaces the current Segment timeline for this track. Continue?",
      )
    ) {
      return;
    }

    setBusy(true);
    setMessage(null);
    try {
      const rows = await invoke<Segment[]>("import_subtitles", {
        mediaId: media.id,
        path,
      });
      onImported(rows);
      setMessage(`Imported ${rows.length} segments.`);
    } catch (errorValue) {
      setMessage(String(errorValue));
    } finally {
      setBusy(false);
    }
  }

  async function exportSubtitle(format: "srt" | "vtt") {
    const defaultName = media.title.replace(/[\\/:*?"<>|]/g, "_");
    let path = await save({
      defaultPath: `${defaultName}.${format}`,
      filters: [
        {
          name: format === "srt" ? "SubRip" : "WebVTT",
          extensions: [format],
        },
      ],
    });
    if (!path) return;
    if (!path.toLowerCase().endsWith(`.${format}`)) {
      path += `.${format}`;
    }

    setBusy(true);
    setMessage(null);
    try {
      await invoke<string>("export_subtitles", {
        mediaId: media.id,
        path,
        textMode,
      });
      setMessage(`Exported ${format.toUpperCase()}.`);
    } catch (errorValue) {
      setMessage(String(errorValue));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="subtitle-actions">
      <button disabled={busy} onClick={() => void importSubtitle()}>
        Import SRT/VTT
      </button>
      <select
        value={textMode}
        onChange={(event) => setTextMode(event.target.value as SubtitleTextMode)}
        title="Export text"
      >
        <option value="source">Original</option>
        <option value="translation">Translation</option>
        <option value="bilingual">Bilingual</option>
      </select>
      <button disabled={busy || segmentCount === 0} onClick={() => void exportSubtitle("srt")}>
        Export SRT
      </button>
      <button disabled={busy || segmentCount === 0} onClick={() => void exportSubtitle("vtt")}>
        Export VTT
      </button>
      {message && <span className="subtitle-message">{message}</span>}
    </div>
  );
}
