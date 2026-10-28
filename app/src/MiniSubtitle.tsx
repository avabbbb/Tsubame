import { useCallback, useEffect, useState } from "react";
import { emitTo, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { MAIN_LABEL, MINI_EVENTS, type MiniSubtitlePayload } from "./miniWindow";

const EMPTY: MiniSubtitlePayload = { source: "", translation: "", hidden: false };

export default function MiniSubtitle() {
  const [payload, setPayload] = useState<MiniSubtitlePayload>(EMPTY);
  const [locked, setLocked] = useState(false);
  const [showTranslation, setShowTranslation] = useState(true);

  // Locked = click-through: the window ignores the mouse, so unlocking is done
  // from the main window (playerbar) which sends set-lock false.
  const applyLock = useCallback(async (next: boolean) => {
    await getCurrentWindow().setIgnoreCursorEvents(next);
    setLocked(next);
    await emitTo(MAIN_LABEL, MINI_EVENTS.locked, next);
  }, []);

  useEffect(() => {
    document.documentElement.style.background = "transparent";
    document.body.style.background = "transparent";
    document.body.style.margin = "0";
    document.body.style.overflow = "hidden";
  }, []);

  useEffect(() => {
    const subscriptions = [
      listen<MiniSubtitlePayload>(MINI_EVENTS.subtitle, (event) => setPayload(event.payload)),
      listen<boolean>(MINI_EVENTS.setLock, (event) => void applyLock(event.payload)),
    ];
    void Promise.all(subscriptions).then(() => emitTo(MAIN_LABEL, MINI_EVENTS.ready, null));
    return () => {
      subscriptions.forEach((pending) => void pending.then((unlisten) => unlisten()));
    };
  }, [applyLock]);

  const close = async () => {
    await emitTo(MAIN_LABEL, MINI_EVENTS.closed, null);
    await getCurrentWindow().close();
  };

  const translation = payload.hidden ? "…" : payload.translation;
  const buttonStyle = {
    background: "rgba(255,255,255,0.12)",
    color: "#fff",
    border: "none",
    borderRadius: 6,
    padding: "2px 8px",
    fontSize: 12,
    cursor: "pointer",
  };

  return (
    <div
      data-tauri-drag-region
      className="mini-subtitle"
      style={{
        boxSizing: "border-box",
        height: "100vh",
        display: "flex",
        flexDirection: "column",
        justifyContent: "center",
        alignItems: "center",
        gap: 4,
        padding: "8px 16px",
        borderRadius: 12,
        background: locked ? "rgba(0,0,0,0.35)" : "rgba(0,0,0,0.6)",
        color: "#fff",
        textAlign: "center",
        textShadow: "0 1px 3px rgba(0,0,0,0.9)",
        userSelect: "none",
        cursor: locked ? "default" : "move",
      }}
    >
      {!locked && (
        <div style={{ position: "absolute", top: 6, right: 8, display: "flex", gap: 4 }}>
          <button type="button" style={buttonStyle} onClick={() => setShowTranslation((value) => !value)}>
            {showTranslation ? "Hide translation" : "Show translation"}
          </button>
          <button
            type="button"
            style={buttonStyle}
            title="Click-through: unlock again from the main window"
            onClick={() => void applyLock(true)}
          >
            Lock
          </button>
          <button type="button" style={buttonStyle} onClick={() => void close()}>
            ✕
          </button>
        </div>
      )}
      <div style={{ fontSize: 22, lineHeight: 1.35, pointerEvents: "none" }}>
        {payload.source || "♪"}
      </div>
      {showTranslation && payload.translation && (
        <div style={{ fontSize: 16, lineHeight: 1.35, opacity: 0.85, pointerEvents: "none" }}>{translation}</div>
      )}
    </div>
  );
}
