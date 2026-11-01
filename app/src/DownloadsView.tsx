import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type { DownloadRow } from "./types";

// Downloads rail (PR #14): the shared queue from download.list, with
// download.cancel / download.retry. Polls while mounted.

const POLL_MS = 1500;

function progress(row: DownloadRow) {
  if (row.status === "succeeded") return "100%";
  if (row.total) return Math.floor((row.bytes_done / row.total) * 100) + "%";
  return row.bytes_done ? (row.bytes_done / 1024 / 1024).toFixed(1) + " MB" : "";
}

export default function DownloadsView({ onImported }: { onImported: () => void }) {
  const [rows, setRows] = useState<DownloadRow[]>([]);
  const [error, setError] = useState<string | null>(null);
  const imported = useRef<number | null>(null);
  // The parent re-renders on every playback tick; keep the poll stable.
  const onImportedRef = useRef(onImported);
  onImportedRef.current = onImported;

  const refresh = useCallback(async () => {
    try {
      const next = await invoke<DownloadRow[]>("download_list", { input: {} });
      setRows(next);
      setError(null);
      const done = next.filter((row) => row.status === "succeeded" && row.import).length;
      if (imported.current !== null && done > imported.current) onImportedRef.current();
      imported.current = done;
    } catch (err) {
      setError(String(err));
    }
  }, []);

  useEffect(() => {
    void refresh();
    const timer = window.setInterval(() => void refresh(), POLL_MS);
    return () => window.clearInterval(timer);
  }, [refresh]);

  const act = async (command: "download_cancel" | "download_retry", id: string) => {
    try {
      await invoke(command, { input: { id } });
    } catch (err) {
      setError(String(err));
    }
    await refresh();
  };

  return (
    <section className="content" style={{ overflowY: "auto", minHeight: 0, padding: 16 }}>
      <p className="eyebrow">DOWNLOADS</p>
      <h2>Download queue</h2>
      {error && <p role="alert">{error}</p>}
      {!rows.length ? (
        <p>Nothing queued. Choose files in Discover to add them here.</p>
      ) : (
        <table style={{ width: "100%", borderCollapse: "collapse" }}>
          <thead>
            <tr style={{ textAlign: "left" }}>
              <th>File</th>
              <th>Status</th>
              <th>Progress</th>
              <th>Attempts</th>
              <th>Error</th>
              <th />
            </tr>
          </thead>
          <tbody>
            {rows.map((row) => (
              <tr key={row.id}>
                <td title={row.dest}>{row.dest.split(/[\\/]/).pop() || row.dest}</td>
                <td>{row.status}</td>
                <td>{progress(row)}</td>
                <td>{row.attempts}</td>
                <td style={{ maxWidth: 260, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={row.error ?? ""}>
                  {row.error ?? ""}
                </td>
                <td>
                  {(row.status === "queued" || row.status === "running") && (
                    <button className="secondary" onClick={() => void act("download_cancel", row.id)}>
                      Cancel
                    </button>
                  )}
                  {(row.status === "failed" || row.status === "cancelled") && (
                    <button className="secondary" onClick={() => void act("download_retry", row.id)}>
                      Retry
                    </button>
                  )}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </section>
  );
}
