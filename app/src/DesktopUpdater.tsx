import { useCallback, useEffect, useRef, useState } from "react";
import { isTauri } from "@tauri-apps/api/core";
import { check, type Update } from "@tauri-apps/plugin-updater";
import { relaunch } from "@tauri-apps/plugin-process";
import { userMessage } from "./errors";

// Preview builds intentionally have no update signing key or updater endpoint.
// Only signed tag releases opt into this capability.
const UPDATES_ENABLED =
  import.meta.env.VITE_YUZUKI_UPDATER_ENABLED === "true" && isTauri();

type Props = { visible: boolean };

export default function DesktopUpdater({ visible }: Props) {
  const candidate = useRef<Update | null>(null);
  const busy = useRef(false);
  // busy (ref) guards re-entry synchronously; isBusy (state) re-renders the buttons.
  const [isBusy, setIsBusy] = useState(false);
  const [status, setStatus] = useState(
    UPDATES_ENABLED ? "Checking for updates…" : "Updates require an official signed release.",
  );
  const [version, setVersion] = useState<string | null>(null);
  const [percent, setPercent] = useState<number | null>(null);

  const checkNow = useCallback(async () => {
    if (!UPDATES_ENABLED || busy.current) return;
    busy.current = true;
    setIsBusy(true);
    setStatus("Checking for updates…");
    setPercent(null);
    try {
      const old = candidate.current;
      candidate.current = null;
      if (old) await old.close();
      const update = await check({ timeout: 15000 });
      if (update) {
        candidate.current = update;
        setVersion(update.version);
        setStatus(`Version ${update.version} is available.`);
      } else {
        setVersion(null);
        setStatus("You're up to date.");
      }
    } catch (error) {
      setVersion(null);
      setStatus(`Update check failed: ${userMessage(error)}`);
    } finally {
      busy.current = false;
      setIsBusy(false);
    }
  }, []);

  useEffect(() => {
    if (!UPDATES_ENABLED) return;
    void checkNow();
    return () => {
      const update = candidate.current;
      candidate.current = null;
      if (update && !busy.current) void update.close().catch(() => undefined);
    };
  }, [checkNow]);

  const installUpdate = async () => {
    const update = candidate.current;
    if (!update || busy.current) return;
    busy.current = true;
    setIsBusy(true);
    setStatus(`Downloading version ${update.version}…`);
    let bytesReceived = 0;
    let totalBytes: number | undefined;
    try {
      await update.downloadAndInstall((event) => {
        if (event.event === "Started") {
          bytesReceived = 0;
          totalBytes = event.data.contentLength;
          setPercent(0);
        } else if (event.event === "Progress") {
          bytesReceived += event.data.chunkLength;
          setPercent(totalBytes ? Math.min(100, Math.round(bytesReceived / totalBytes * 100)) : null);
        } else if (event.event === "Finished") {
          setPercent(100);
        }
      });
      candidate.current = null;
      setStatus("Update installed. Restarting…");
      await relaunch();
    } catch (error) {
      setStatus(`Update installation failed: ${userMessage(error)}`);
    } finally {
      busy.current = false;
      setIsBusy(false);
    }
  };

  if (!visible) return null;

  return (
    <section className="settings-summary-card" aria-label="YUZUKI updates">
      <div className="settings-summary-row">
        <div className="settings-summary-copy">
          <strong>Updates</strong>
          <small aria-live="polite">{status}</small>
        </div>
        <div className="settings-summary-actions">
          {version && (
            <button className="primary" disabled={isBusy} onClick={() => void installUpdate()}>
              Install {version}
            </button>
          )}
          <button className="secondary" disabled={!UPDATES_ENABLED || isBusy} onClick={() => void checkNow()}>
            Check now
          </button>
        </div>
      </div>
      {percent !== null && <progress value={percent} max={100} className="settings-summary-progress" />}
      <details className="settings-summary-more">
        <summary>How updates work</summary>
        <p>
          Only published releases are offered. Updates are signed and verified before installation.
          No installation starts without your confirmation.
        </p>
      </details>
    </section>
  );
}
