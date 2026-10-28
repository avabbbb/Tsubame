import { useState, type KeyboardEvent as ReactKeyboardEvent } from "react";
import {
  GLOBAL_ACTIONS,
  LOCAL_ACTIONS,
  acceleratorFromEvent,
  comboFromEvent,
  defaultKeymap,
  displayCombo,
  findConflicts,
  type GlobalAction,
  type Keymap,
  type LocalAction,
} from "./keymap";

type Capture = { scope: "local"; id: LocalAction } | { scope: "global"; id: GlobalAction };

type Props = {
  keymap: Keymap;
  onChange: (next: Keymap) => void;
  error: string | null;
};

export default function ShortcutSettings({ keymap, onChange, error }: Props) {
  const [capture, setCapture] = useState<Capture | null>(null);
  const [hint, setHint] = useState<string | null>(null);
  const localConflicts = findConflicts(keymap.local);
  const globalConflicts = findConflicts(keymap.global);

  const onCaptureKey = (event: ReactKeyboardEvent<HTMLButtonElement>) => {
    if (!capture) return;
    event.preventDefault();
    event.stopPropagation();
    if (event.key === "Escape" && !event.shiftKey && !event.ctrlKey && !event.altKey && !event.metaKey) {
      setCapture(null);
      setHint(null);
      return;
    }
    const clear = event.key === "Backspace" || event.key === "Delete";
    if (capture.scope === "local") {
      const combo = clear ? "" : comboFromEvent(event.nativeEvent);
      if (combo === null) return;
      onChange({ ...keymap, local: { ...keymap.local, [capture.id]: combo } });
    } else {
      const accelerator = clear ? "" : acceleratorFromEvent(event.nativeEvent);
      if (accelerator === null) {
        setHint("System-wide shortcuts need Ctrl/⌘ or Alt.");
        return;
      }
      onChange({ ...keymap, global: { ...keymap.global, [capture.id]: accelerator } });
    }
    setCapture(null);
    setHint(null);
  };

  const isCapturing = (scope: Capture["scope"], id: string) =>
    capture !== null && capture.scope === scope && capture.id === id;

  const bindingButton = (scope: Capture["scope"], id: LocalAction | GlobalAction, combo: string, conflict: boolean) => (
    <button
      type="button"
      data-shortcut-capture
      className={"shortcut-binding" + (conflict ? " conflict" : "")}
      title={conflict ? "Also bound to another action" : "Click, then press the new keys (Backspace clears, Esc cancels)"}
      style={{ minWidth: 150, color: conflict ? "var(--danger, #d9534f)" : undefined }}
      onClick={() => {
        setHint(null);
        setCapture(scope === "local" ? { scope, id: id as LocalAction } : { scope, id: id as GlobalAction });
      }}
      onKeyDown={onCaptureKey}
      onBlur={() => setCapture(null)}
    >
      {isCapturing(scope, id) ? "Press keys…" : displayCombo(combo)}
    </button>
  );

  const rowStyle = { display: "flex", justifyContent: "space-between", alignItems: "center", gap: 12, padding: "4px 0" };

  return (
    <section className="shortcut-settings provider-settings">
      <header className="provider-settings-head">
        <div>
          <p className="eyebrow">KEYBOARD</p>
          <h2>Shortcuts</h2>
          <p className="muted">Click a binding and press the new keys. Backspace clears it, Esc cancels.</p>
        </div>
        <button type="button" onClick={() => onChange(defaultKeymap())}>
          Reset to defaults
        </button>
      </header>

      <h3>In the app window</h3>
      {LOCAL_ACTIONS.map(({ id, label }) => (
        <div key={id} style={rowStyle}>
          <span>{label}</span>
          {bindingButton("local", id, keymap.local[id], localConflicts.has(id))}
        </div>
      ))}
      {localConflicts.size > 0 && (
        <p className="muted">Highlighted shortcuts share keys; only the first listed action runs.</p>
      )}

      <h3>System-wide</h3>
      <label style={{ display: "flex", gap: 8, alignItems: "center" }}>
        <input
          type="checkbox"
          checked={keymap.globalEnabled}
          onChange={(event) => onChange({ ...keymap, globalEnabled: event.target.checked })}
        />
        Work even when YUZUKI is in the background
      </label>
      {GLOBAL_ACTIONS.map(({ id, label }) => (
        <div key={id} style={{ ...rowStyle, opacity: keymap.globalEnabled ? 1 : 0.55 }}>
          <span>{label}</span>
          {bindingButton("global", id, keymap.global[id], globalConflicts.has(id))}
        </div>
      ))}
      {hint && <p className="muted">{hint}</p>}
      {error && <p role="alert" style={{ color: "var(--danger, #d9534f)" }}>{error}</p>}
    </section>
  );
}
