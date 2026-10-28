// Configurable keyboard shortcuts (P5).
//
// In-window shortcuts are matched against a normalized combo string built from
// KeyboardEvent.code (layout-stable), e.g. "Shift+ArrowLeft", "H", "BracketLeft".
// System-wide shortcuts use Tauri accelerator strings and are registered with
// @tauri-apps/plugin-global-shortcut only when the user enables them, so the
// app never steals keys from other programs by default.

export type LocalAction =
  | "togglePlay"
  | "seekBack"
  | "seekForward"
  | "seekBackLong"
  | "seekForwardLong"
  | "prevSegment"
  | "nextSegment"
  | "sentenceLoop"
  | "loopA"
  | "loopB"
  | "clearLoop"
  | "revealHold"
  | "toggleBlind"
  | "toggleAutoPause"
  | "cycleContinue";

export type GlobalAction =
  | "togglePlay"
  | "prevSegment"
  | "nextSegment"
  | "sentenceLoop"
  | "toggleBlind";

export const LOCAL_ACTIONS: { id: LocalAction; label: string }[] = [
  { id: "togglePlay", label: "Play / pause" },
  { id: "seekBack", label: "Back 5 s" },
  { id: "seekForward", label: "Forward 5 s" },
  { id: "seekBackLong", label: "Back 15 s" },
  { id: "seekForwardLong", label: "Forward 15 s" },
  { id: "prevSegment", label: "Previous sentence" },
  { id: "nextSegment", label: "Next sentence" },
  { id: "sentenceLoop", label: "Loop current sentence" },
  { id: "loopA", label: "Set loop point A" },
  { id: "loopB", label: "Set loop point B" },
  { id: "clearLoop", label: "Clear loop" },
  { id: "revealHold", label: "Hold to reveal translation" },
  { id: "toggleBlind", label: "Toggle blind listening" },
  { id: "toggleAutoPause", label: "Toggle auto-pause" },
  { id: "cycleContinue", label: "Cycle continuous play" },
];

export const GLOBAL_ACTIONS: { id: GlobalAction; label: string }[] = [
  { id: "togglePlay", label: "Play / pause" },
  { id: "prevSegment", label: "Previous sentence" },
  { id: "nextSegment", label: "Next sentence" },
  { id: "sentenceLoop", label: "Loop current sentence" },
  { id: "toggleBlind", label: "Toggle blind listening" },
];

export const DEFAULT_LOCAL: Record<LocalAction, string> = {
  togglePlay: "Space",
  seekBack: "ArrowLeft",
  seekForward: "ArrowRight",
  seekBackLong: "Shift+ArrowLeft",
  seekForwardLong: "Shift+ArrowRight",
  prevSegment: "BracketLeft",
  nextSegment: "BracketRight",
  sentenceLoop: "L",
  loopA: "Shift+A",
  loopB: "Shift+B",
  clearLoop: "Escape",
  revealHold: "H",
  toggleBlind: "Shift+H",
  toggleAutoPause: "P",
  cycleContinue: "N",
};

export const DEFAULT_GLOBAL: Record<GlobalAction, string> = {
  togglePlay: "CommandOrControl+Alt+Space",
  prevSegment: "CommandOrControl+Alt+Left",
  nextSegment: "CommandOrControl+Alt+Right",
  sentenceLoop: "CommandOrControl+Alt+L",
  toggleBlind: "CommandOrControl+Alt+H",
};

export type Keymap = {
  local: Record<LocalAction, string>;
  global: Record<GlobalAction, string>;
  globalEnabled: boolean;
};

export const KEYMAP_KEY = "yuzuki-keymap-v1";

export function defaultKeymap(): Keymap {
  return { local: { ...DEFAULT_LOCAL }, global: { ...DEFAULT_GLOBAL }, globalEnabled: false };
}

export function loadKeymap(): Keymap {
  const base = defaultKeymap();
  try {
    const saved = JSON.parse(localStorage.getItem(KEYMAP_KEY) || "{}") as Partial<Keymap>;
    for (const { id } of LOCAL_ACTIONS) {
      const value = saved.local?.[id];
      if (typeof value === "string") base.local[id] = value;
    }
    for (const { id } of GLOBAL_ACTIONS) {
      const value = saved.global?.[id];
      if (typeof value === "string") base.global[id] = value;
    }
    base.globalEnabled = saved.globalEnabled === true;
  } catch {
    // Corrupt storage falls back to defaults.
  }
  return base;
}

export function saveKeymap(keymap: Keymap): void {
  localStorage.setItem(KEYMAP_KEY, JSON.stringify(keymap));
}

const MODIFIER_CODES: Record<string, true> = {
  ShiftLeft: true,
  ShiftRight: true,
  ControlLeft: true,
  ControlRight: true,
  AltLeft: true,
  AltRight: true,
  MetaLeft: true,
  MetaRight: true,
};

/** Key part of a combo from KeyboardEvent.code: KeyA -> A, Digit1 -> 1. */
export function keyFromCode(code: string): string {
  if (/^Key[A-Z]$/.test(code)) return code.slice(3);
  if (/^Digit[0-9]$/.test(code)) return code.slice(5);
  return code;
}

/** Normalized combo for an event, or null for a bare modifier press. */
export function comboFromEvent(event: Pick<KeyboardEvent, "code" | "ctrlKey" | "metaKey" | "altKey" | "shiftKey">): string | null {
  if (!event.code || MODIFIER_CODES[event.code]) return null;
  const parts: string[] = [];
  if (event.ctrlKey || event.metaKey) parts.push("Mod");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");
  parts.push(keyFromCode(event.code));
  return parts.join("+");
}

/** The key part of a combo ("Shift+H" -> "H"), used to release hold actions. */
export function comboKey(combo: string): string {
  const parts = combo.split("+");
  return parts[parts.length - 1] ?? "";
}

export function matchLocal(keymap: Keymap, combo: string): LocalAction | null {
  for (const { id } of LOCAL_ACTIONS) {
    if (keymap.local[id] === combo) return id;
  }
  return null;
}

/** Actions sharing a binding with another action (empty bindings are ignored). */
export function findConflicts<T extends string>(bindings: Record<T, string>): Set<T> {
  const byCombo = new Map<string, T[]>();
  for (const [id, combo] of Object.entries(bindings) as [T, string][]) {
    if (!combo) continue;
    const key = combo.toLowerCase();
    byCombo.set(key, [...(byCombo.get(key) ?? []), id]);
  }
  const conflicts = new Set<T>();
  for (const ids of byCombo.values()) {
    if (ids.length > 1) ids.forEach((id) => conflicts.add(id));
  }
  return conflicts;
}

const ACCEL_KEYS: Record<string, string> = {
  ArrowLeft: "Left",
  ArrowRight: "Right",
  ArrowUp: "Up",
  ArrowDown: "Down",
};

/** Tauri accelerator for a captured event; globals require a modifier. */
export function acceleratorFromEvent(event: Pick<KeyboardEvent, "code" | "ctrlKey" | "metaKey" | "altKey" | "shiftKey">): string | null {
  if (!event.code || MODIFIER_CODES[event.code]) return null;
  if (!(event.ctrlKey || event.metaKey || event.altKey)) return null;
  const parts: string[] = [];
  if (event.ctrlKey || event.metaKey) parts.push("CommandOrControl");
  if (event.altKey) parts.push("Alt");
  if (event.shiftKey) parts.push("Shift");
  const key = keyFromCode(event.code);
  parts.push(ACCEL_KEYS[key] ?? key);
  return parts.join("+");
}

/** Human-readable label for a combo or accelerator. */
export function displayCombo(combo: string): string {
  if (!combo) return "Unassigned";
  const isMac = typeof navigator !== "undefined" && /Mac/i.test(navigator.platform);
  return combo
    .split("+")
    .map((part) => {
      if (part === "Mod" || part === "CommandOrControl") return isMac ? "⌘" : "Ctrl";
      if (part === "Alt") return isMac ? "⌥" : "Alt";
      if (part === "BracketLeft") return "[";
      if (part === "BracketRight") return "]";
      if (part === "ArrowLeft" || part === "Left") return "←";
      if (part === "ArrowRight" || part === "Right") return "→";
      if (part === "ArrowUp" || part === "Up") return "↑";
      if (part === "ArrowDown" || part === "Down") return "↓";
      if (part === "Escape") return "Esc";
      return part;
    })
    .join(" + ");
}
