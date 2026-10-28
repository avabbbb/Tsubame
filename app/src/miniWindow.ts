import { WebviewWindow } from "@tauri-apps/api/webviewWindow";
import { invoke } from "@tauri-apps/api/core";

// P6 floating mini subtitle window. The main window owns playback and pushes
// the current sentence; the mini window only renders and reports lock state.
export const MINI_LABEL = "mini";
export const MAIN_LABEL = "main";

export const MINI_EVENTS = {
  subtitle: "yuzuki://mini/subtitle",
  ready: "yuzuki://mini/ready",
  locked: "yuzuki://mini/locked",
  setLock: "yuzuki://mini/set-lock",
  closed: "yuzuki://mini/closed",
} as const;

export type MiniSubtitlePayload = {
  source: string;
  translation: string;
  /** Blind listening: translation hidden unless the reveal key is held. */
  hidden: boolean;
};

export function isMiniView(): boolean {
  return new URLSearchParams(window.location.search).get("mini") === "1";
}

export async function openMiniWindow(): Promise<void> {
  const existing = await WebviewWindow.getByLabel(MINI_LABEL);
  if (existing) {
    await existing.show();
    await existing.setFocus();
    return;
  }
  // Same WebView2 profile as the main window (under the data root).
  const dataDirectory = await invoke<string | null>("webview_data_directory");
  const mini = new WebviewWindow(MINI_LABEL, {
    url: "index.html?mini=1",
    title: "YUZUKI subtitles",
    width: 760,
    height: 150,
    minWidth: 320,
    minHeight: 80,
    alwaysOnTop: true,
    decorations: false,
    transparent: true,
    skipTaskbar: true,
    resizable: true,
    focus: false,
    ...(dataDirectory ? { dataDirectory } : {}),
  });
  await new Promise<void>((resolve, reject) => {
    void mini.once("tauri://created", () => resolve());
    void mini.once<string>("tauri://error", (event) => reject(new Error(String(event.payload))));
  });
}

export async function closeMiniWindow(): Promise<void> {
  const existing = await WebviewWindow.getByLabel(MINI_LABEL);
  await existing?.close();
}
