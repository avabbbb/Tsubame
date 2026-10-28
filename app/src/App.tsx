import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type CSSProperties,
  type ChangeEvent,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { emitTo, listen } from "@tauri-apps/api/event";
import { open } from "@tauri-apps/plugin-dialog";
import AsrPanel from "./AsrPanel";
import DubExportActions from "./DubExportActions";
import RefinePanel from "./RefinePanel";
import TranslatePanel from "./TranslatePanel";
import ProviderSettings from "./ProviderSettings";
import DesktopUpdater from "./DesktopUpdater";
import VoiceProfileSettings from "./VoiceProfileSettings";

const SETTINGS_TABS = [
  { id: "providers", label: "Providers" },
  { id: "voices", label: "Voices" },
  { id: "shortcuts", label: "Shortcuts" },
  { id: "dictionary", label: "Dictionary" },
  { id: "updates", label: "Updates" },
] as const;
type SettingsTab = (typeof SETTINGS_TABS)[number]["id"];
import SegmentInspector from "./SegmentInspector";
import { pushedLabel } from "./regenerate";
import { register as registerGlobalShortcut, unregister as unregisterGlobalShortcut } from "@tauri-apps/plugin-global-shortcut";
import {
  GLOBAL_ACTIONS,
  comboFromEvent,
  comboKey,
  keyFromCode,
  loadKeymap,
  matchLocal,
  saveKeymap,
  type Keymap,
  type LocalAction,
} from "./keymap";
import ShortcutSettings from "./ShortcutSettings";
import DictionaryPanel, { DictResults, lookupWord, type DictLookupResult } from "./DictionaryPanel";
import {
  MINI_EVENTS,
  MINI_LABEL,
  closeMiniWindow,
  openMiniWindow,
  type MiniSubtitlePayload,
} from "./miniWindow";
import SubtitleActions from "./SubtitleActions";
import WaveformEditor from "./WaveformEditor";
import GetStarted, { type GetStartedProgress } from "./GetStarted";
import JobsView from "./JobsView";
import type {
  DubPlan,
  MediaItem,
  RailSection,
  Segment,
  ThemePreference,
} from "./types";

const UI_KEY = "tsubame-workbench-v2";
const RAILS: RailSection[] = [
  "home",
  "library",
  "discover",
  "downloads",
  "studio",
  "jobs",
  "settings",
];

const RAIL_ICONS: Record<RailSection, string> = {
  home: "⌂",
  library: "▣",
  discover: "⌕",
  downloads: "↓",
  studio: "✦",
  jobs: "≡",
  settings: "⚙",
};

type UiState = {
  activeRail: RailSection;
  sidebarWidth: number;
  inspectorWidth: number;
  sidebarVisible: boolean;
  inspectorVisible: boolean;
  openTabIds: number[];
  activeMediaId: number | null;
  theme: ThemePreference;
};

// Listening modes (ASPlayer parity, docs/13): kept apart from layout state.
const LISTEN_KEY = "yuzuki-listening-v1";
// Pause this early so a 250 ms timeupdate tick rarely spills into the next sentence.
const AUTO_PAUSE_LEAD_MS = 80;

type ContinueMode = "off" | "next" | "loop-list";

type ListenPrefs = {
  autoPause: boolean;
  blindMode: boolean;
  continueMode: ContinueMode;
};

function readListenPrefs(): Partial<ListenPrefs> {
  try {
    return JSON.parse(localStorage.getItem(LISTEN_KEY) || "{}") as Partial<ListenPrefs>;
  } catch {
    return {};
  }
}

function readUiState(): Partial<UiState> {
  try {
    return JSON.parse(localStorage.getItem(UI_KEY) || "{}") as Partial<UiState>;
  } catch {
    return {};
  }
}

function clamp(value: number, min: number, max: number) {
  return Math.max(min, Math.min(max, value));
}

function formatTime(value: number) {
  const total = Math.max(0, Math.floor(value || 0));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  if (hours) {
    return `${hours}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
  }
  return `${minutes}:${String(seconds).padStart(2, "0")}`;
}

function isEditableTarget(target: EventTarget | null) {
  const element = target as HTMLElement | null;
  if (!element) return false;
  return (
    element.isContentEditable ||
    ["INPUT", "TEXTAREA", "SELECT"].includes(element.tagName)
  );
}

export default function App() {
  const initial = useMemo(readUiState, []);
  const prefersDark = useMemo(
    () => window.matchMedia("(prefers-color-scheme: dark)"),
    [],
  );

  const [items, setItems] = useState<MediaItem[]>([]);
  const [current, setCurrent] = useState<MediaItem | null>(null);
  const [segments, setSegments] = useState<Segment[]>([]);
  const [selected, setSelected] = useState<Segment | null>(null);
  const [dubPlan, setDubPlan] = useState<DubPlan | null>(null);
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [playbackRate, setPlaybackRate] = useState(1);
  const [volume, setVolume] = useState(1);
  const [activeRail, setActiveRail] = useState<RailSection>(
    initial.activeRail ?? "library",
  );
  const [openTabIds, setOpenTabIds] = useState<number[]>(
    initial.openTabIds ?? [],
  );
  const [sidebarWidth, setSidebarWidth] = useState(initial.sidebarWidth ?? 238);
  const [inspectorWidth, setInspectorWidth] = useState(
    initial.inspectorWidth ?? 310,
  );
  const [sidebarVisible, setSidebarVisible] = useState(
    initial.sidebarVisible ?? true,
  );
  const [inspectorVisible, setInspectorVisible] = useState(
    initial.inspectorVisible ?? true,
  );
  const [theme, setTheme] = useState<ThemePreference>(
    initial.theme ?? "system",
  );
  const [systemDark, setSystemDark] = useState(prefersDark.matches);
  const [sentenceLoop, setSentenceLoop] = useState(false);
  const [loopA, setLoopA] = useState<number | null>(null);
  const [settingsTab, setSettingsTab] = useState<SettingsTab>("providers");
  // First-run checklist progress, derived only from real app state.
  const getStartedProgress = useMemo<GetStartedProgress>(() => {
    const transcript = segments.length > 0;
    const translation = segments.some((segment) => segment.translated_text.trim() !== "");
    const regenerate = segments.some((segment) => segment.tts_asset_key !== null);
    return {
      import: items.length > 0,
      asr: transcript,
      transcript,
      edit: segments.some((segment) => segment.revision > 1),
      translationProvider: translation,
      translation,
      ttsProvider: regenerate,
      regenerate,
      dubbed: (dubPlan?.placements.length ?? 0) > 0,
    };
  }, [items.length, segments, dubPlan]);
  const [loopB, setLoopB] = useState<number | null>(null);

  const mediaRef = useRef<HTMLMediaElement | null>(null);
  const [mediaElement, setMediaElement] = useState<HTMLMediaElement | null>(null);
  const pendingResumeMsRef = useRef(0);
  const lastPersistAtRef = useRef(0);

  const sourceUrl = useMemo(
    () => (current ? convertFileSrc(current.path) : ""),
    [current],
  );

  const openTabs = useMemo(
    () =>
      openTabIds
        .map((id) => items.find((item) => item.id === id))
        .filter((item): item is MediaItem => Boolean(item)),
    [items, openTabIds],
  );

  const mediaWorkspaceVisible =
    activeRail === "library" || activeRail === "studio";

  const [dictPopup, setDictPopup] = useState<{ query: string; result: DictLookupResult | null; x: number; y: number } | null>(null);
  const lookupSelection = async (event: { clientX: number; clientY: number }) => {
    const text = window.getSelection()?.toString().trim() ?? "";
    if (!text || text.length > 40) return;
    try {
      const result = await lookupWord(text);
      setDictPopup({ query: text, result, x: event.clientX, y: event.clientY });
    } catch {
      // No dictionary imported yet: selection stays a plain selection.
    }
  };
  const activeSegment = useMemo(() => {
    const nowMs = currentTime * 1000;
    return (
      segments.find(
        (segment) => nowMs >= segment.start_ms && nowMs < segment.end_ms,
      ) ?? null
    );
  }, [currentTime, segments]);

  const listenInitial = useMemo(readListenPrefs, []);
  const [autoPause, setAutoPause] = useState(listenInitial.autoPause ?? false);
  const [blindMode, setBlindMode] = useState(listenInitial.blindMode ?? false);
  const [revealHeld, setRevealHeld] = useState(false);
  const [continueMode, setContinueMode] = useState<ContinueMode>(
    listenInitial.continueMode ?? "off",
  );
  const lastTickMsRef = useRef(0);
  const autoplayNextRef = useRef(false);
  const translationHidden = blindMode && !revealHeld;

  // P6: floating mini subtitle window fed from the main window's playback.
  const [miniOpen, setMiniOpen] = useState(false);
  const [miniLocked, setMiniLocked] = useState(false);
  const miniPayload = useMemo<MiniSubtitlePayload>(
    () => ({
      source: activeSegment?.source_text ?? "",
      translation: activeSegment?.translated_text ?? "",
      hidden: translationHidden,
    }),
    [activeSegment?.source_text, activeSegment?.translated_text, translationHidden],
  );
  const miniPayloadRef = useRef(miniPayload);
  useEffect(() => {
    miniPayloadRef.current = miniPayload;
    if (miniOpen) void emitTo(MINI_LABEL, MINI_EVENTS.subtitle, miniPayload).catch(() => undefined);
  }, [miniOpen, miniPayload]);
  useEffect(() => {
    const subscriptions = [
      listen(MINI_EVENTS.ready, () => {
        setMiniOpen(true);
        void emitTo(MINI_LABEL, MINI_EVENTS.subtitle, miniPayloadRef.current).catch(() => undefined);
      }),
      listen<boolean>(MINI_EVENTS.locked, (event) => setMiniLocked(event.payload)),
      listen(MINI_EVENTS.closed, () => {
        setMiniOpen(false);
        setMiniLocked(false);
      }),
    ];
    return () => {
      subscriptions.forEach((pending) => void pending.then((unlisten) => unlisten()));
    };
  }, []);
  const toggleMini = useCallback(async () => {
    if (miniOpen) {
      await closeMiniWindow();
      setMiniOpen(false);
      setMiniLocked(false);
    } else {
      await openMiniWindow();
    }
  }, [miniOpen]);
  const unlockMini = useCallback(() => {
    void emitTo(MINI_LABEL, MINI_EVENTS.setLock, false).catch(() => undefined);
  }, []);

  useEffect(() => {
    const prefs: ListenPrefs = { autoPause, blindMode, continueMode };
    localStorage.setItem(LISTEN_KEY, JSON.stringify(prefs));
  }, [autoPause, blindMode, continueMode]);

  const [libraryQuery, setLibraryQuery] = useState("");
  const [librarySort, setLibrarySort] = useState<"added" | "title" | "duration">("added");
  const visibleItems = useMemo(() => {
    const query = libraryQuery.trim().toLowerCase();
    const rows = query
      ? items.filter(
          (item) =>
            item.title.toLowerCase().includes(query) ||
            item.path.toLowerCase().includes(query),
        )
      : items;
    // list_media already returns newest first.
    if (librarySort === "added") return rows;
    return [...rows].sort((a, b) =>
      librarySort === "title"
        ? a.title.localeCompare(b.title, undefined, { numeric: true })
        : a.duration_ms - b.duration_ms,
    );
  }, [items, libraryQuery, librarySort]);

  const resolvedTheme =
    theme === "system" ? (systemDark ? "dark" : "light") : theme;

  const themeIcon =
    theme === "system" ? "◐" : theme === "dark" ? "☾" : "☀";

  const workbenchStyle = {
    "--sidebar-width": sidebarVisible ? `${sidebarWidth}px` : "0px",
    "--inspector-width": inspectorVisible && activeRail !== "settings"
      ? `${inspectorWidth}px`
      : "0px",
  } as CSSProperties;

  const persistPlayback = useCallback(
    async (force = false) => {
      const media = mediaRef.current;
      if (!current || !media) return;

      const now = Date.now();
      if (!force && now - lastPersistAtRef.current < 1800) return;
      lastPersistAtRef.current = now;

      const positionMs = Math.round(media.currentTime * 1000);
      setCurrent((previous) =>
        previous && previous.id === current.id
          ? {
              ...previous,
              playback_position: positionMs,
              speed: playbackRate,
              volume,
            }
          : previous,
      );

      await invoke("save_playback_state", {
        id: current.id,
        positionMs,
        speed: playbackRate,
        volume,
      }).catch(() => undefined);
    },
    [current, playbackRate, volume],
  );

  const refreshLibrary = useCallback(async () => {
    const mediaItems = await invoke<MediaItem[]>("list_media");
    setItems(mediaItems);
    setOpenTabIds((ids) =>
      ids.filter((id) => mediaItems.some((item) => item.id === id)),
    );
    return mediaItems;
  }, []);

  // Imports from any surface (CLI/MCP media.import_folder) refresh the library.
  useEffect(() => {
    const pending = listen("library://changed", () => void refreshLibrary());
    return () => void pending.then((unlisten) => unlisten());
  }, [refreshLibrary]);

  const choose = useCallback(
    async (item: MediaItem, addTab = true) => {
      if (current?.id !== item.id) {
        await persistPlayback(true);
      }

      setCurrent(item);
      setSelected(null);
      setPlaybackRate(item.speed || 1);
      setVolume(Number.isFinite(item.volume) ? item.volume : 1);
      pendingResumeMsRef.current = item.playback_position;
      setCurrentTime(item.playback_position / 1000);
      setDuration(item.duration_ms / 1000);

      const trackSegments = await invoke<Segment[]>("get_segments", {
        mediaId: item.id,
      });
      setSegments(trackSegments);

      if (addTab) {
        setOpenTabIds((ids) =>
          ids.includes(item.id) ? ids : [...ids, item.id],
        );
      }
    },
    [current?.id, persistPlayback],
  );

  const importFiles = useCallback(async () => {
    const picked = await open({
      multiple: true,
      filters: [
        {
          name: "Media",
          extensions: [
            "mp3",
            "m4a",
            "wav",
            "flac",
            "ogg",
            "opus",
            "aac",
            "m4b",
            "mp4",
            "m4v",
            "webm",
            "mkv",
            "mov",
            "avi",
          ],
        },
      ],
    });

    if (!picked) return;
    const paths = Array.isArray(picked) ? picked : [picked];
    for (const path of paths) {
      await invoke("import_media", { path });
    }

    const mediaItems = await refreshLibrary();
    if (mediaItems[0]) {
      await choose(mediaItems[0]);
    }
  }, [choose, refreshLibrary]);

  const [sourceList, setSourceList] = useState<{ id: string; label: string }[]>([]);
  useEffect(() => {
    invoke<{ id: string; label: string }[]>("list_sources").then(setSourceList, () => setSourceList([]));
  }, []);

  const importFolder = useCallback(async () => {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked !== "string") return;
    const imported = await invoke<MediaItem[]>("import_folder", { path: picked });
    await refreshLibrary();
    if (imported[0]) await choose(imported[0]);
  }, [choose, refreshLibrary]);

  const closeTab = useCallback(
    async (id: number) => {
      const index = openTabIds.indexOf(id);
      if (index < 0) return;

      const remaining = openTabIds.filter((tabId) => tabId !== id);
      setOpenTabIds(remaining);

      if (current?.id !== id) return;

      await persistPlayback(true);
      const fallbackId =
        remaining[Math.min(index, Math.max(remaining.length - 1, 0))] ?? null;
      const fallback = items.find((item) => item.id === fallbackId);

      if (fallback) {
        await choose(fallback, false);
      } else {
        setCurrent(null);
        setSegments([]);
        setSelected(null);
        setPlaying(false);
      }
    },
    [choose, current?.id, items, openTabIds, persistPlayback],
  );

  const seekTo = useCallback(
    (seconds: number) => {
      const media = mediaRef.current;
      if (!media) return;
      const max = Number.isFinite(media.duration) ? media.duration : duration;
      media.currentTime = clamp(seconds, 0, Math.max(0, max || 0));
      setCurrentTime(media.currentTime);
    },
    [duration],
  );

  const seekBy = useCallback(
    (seconds: number) => {
      seekTo(currentTime + seconds);
    },
    [currentTime, seekTo],
  );

  const chooseSegment = useCallback(
    (segment: Segment) => {
      setSelected(segment);
      seekTo(segment.start_ms / 1000);
    },
    [seekTo],
  );

  const handleSegmentUpdated = useCallback((updated: Segment) => {
    setSegments((rows) =>
      rows.map((row) => (row.id === updated.id ? updated : row)),
    );
    setSelected((currentSelection) =>
      currentSelection?.id === updated.id ? updated : currentSelection,
    );
  }, []);

  // Regenerate / take switches move later dubs and set their pushed marks.
  const refreshSegmentsById = useCallback(
    (ids: number[]) => {
      for (const id of ids) {
        void invoke<Segment | null>("get_segment", { id })
          .then((row) => {
            if (row) handleSegmentUpdated(row);
          })
          .catch(() => undefined);
      }
    },
    [handleSegmentUpdated],
  );

  const commitSegmentTiming = useCallback(
    async (segment: Segment, startMs: number, endMs: number) => {
      try {
        const updated = await invoke<Segment>("update_segment", {
          id: segment.id,
          expectedRevision: segment.revision,
          sourceText: segment.source_text,
          translatedText: segment.translated_text,
          startMs,
          endMs,
        });
        handleSegmentUpdated(updated);
      } catch (errorValue) {
        const latest = await invoke<Segment | null>("get_segment", {
          id: segment.id,
        }).catch(() => null);
        if (latest) {
          handleSegmentUpdated(latest);
          setSelected(latest);
        }
        console.warn("Could not commit waveform timing change", errorValue);
      }
    },
    [handleSegmentUpdated],
  );

  const handleSubtitleImported = useCallback((rows: Segment[]) => {
    setSegments(rows);
    setSelected(null);
  }, []);

  const togglePlayback = useCallback(async () => {
    const media = mediaRef.current;
    if (!media || !current) return;
    if (media.paused) await media.play();
    else media.pause();
  }, [current]);

  const stepSegment = useCallback(
    (direction: -1 | 1) => {
      if (!segments.length) return;
      const nowMs = currentTime * 1000;
      let index = segments.findIndex(
        (segment) => nowMs >= segment.start_ms && nowMs < segment.end_ms,
      );
      if (index < 0) {
        index = segments.findIndex((segment) => segment.start_ms > nowMs);
        if (index < 0) index = segments.length - 1;
      }
      const next = clamp(index + direction, 0, segments.length - 1);
      chooseSegment(segments[next]);
    },
    [chooseSegment, currentTime, segments],
  );

  const toggleSentenceLoop = useCallback(() => {
    setSentenceLoop((enabled) => {
      const next = !enabled;
      if (next) {
        setLoopA(null);
        setLoopB(null);
      }
      return next;
    });
  }, []);

  const setLoopPoint = useCallback(
    (point: "a" | "b") => {
      if (point === "a") {
        setLoopA(currentTime);
        setLoopB((value) =>
          value !== null && value <= currentTime ? null : value,
        );
      } else {
        setLoopB(currentTime);
        setLoopA((value) =>
          value !== null && currentTime <= value ? null : value,
        );
      }
      setSentenceLoop(false);
    },
    [currentTime],
  );

  const clearLoop = useCallback(() => {
    setLoopA(null);
    setLoopB(null);
    setSentenceLoop(false);
  }, []);

  const applyPlaybackSettings = useCallback(
    (nextRate: number, nextVolume: number) => {
      const media = mediaRef.current;
      if (media) {
        media.playbackRate = nextRate;
        media.volume = nextVolume;
      }
    },
    [],
  );

  const onRate = useCallback(
    (event: ChangeEvent<HTMLSelectElement>) => {
      const next = Number(event.target.value);
      setPlaybackRate(next);
      applyPlaybackSettings(next, volume);
    },
    [applyPlaybackSettings, volume],
  );

  const onVolume = useCallback(
    (event: ChangeEvent<HTMLInputElement>) => {
      const next = Number(event.target.value);
      setVolume(next);
      applyPlaybackSettings(playbackRate, next);
    },
    [applyPlaybackSettings, playbackRate],
  );

  const onTimeUpdate = useCallback(() => {
    const media = mediaRef.current;
    if (!media) return;

    const now = media.currentTime;
    setCurrentTime(now);
    const prevMs = lastTickMsRef.current;
    lastTickMsRef.current = now * 1000;

    if (loopA !== null && loopB !== null && now >= loopB) {
      media.currentTime = loopA;
      return;
    }

    if (sentenceLoop) {
      const nowMs = now * 1000;
      const segment = segments.find(
        (row) => nowMs >= row.start_ms && nowMs < row.end_ms,
      );
      if (segment && nowMs >= segment.end_ms - 25) {
        media.currentTime = segment.start_ms / 1000;
        return;
      }
    }

    // Auto-pause at sentence end: only a continuous crossing of an end counts,
    // so seeking or resuming right after the pause never pauses again.
    if (autoPause && !sentenceLoop && !media.paused) {
      const nowMs = now * 1000;
      const continuous = nowMs >= prevMs && nowMs - prevMs < 1500;
      const ended =
        continuous &&
        segments.some(
          (segment) =>
            prevMs < segment.end_ms - AUTO_PAUSE_LEAD_MS &&
            nowMs >= segment.end_ms - AUTO_PAUSE_LEAD_MS,
        );
      if (ended) {
        media.pause();
        return;
      }
    }

    void persistPlayback();
  }, [autoPause, loopA, loopB, persistPlayback, segments, sentenceLoop]);

  const onLoadedMetadata = useCallback(() => {
    const media = mediaRef.current;
    if (!media || !current) return;

    const nextDuration = Number.isFinite(media.duration) ? media.duration : 0;
    setDuration(nextDuration);
    media.playbackRate = playbackRate;
    media.volume = volume;

    if (pendingResumeMsRef.current > 0 && nextDuration > 0) {
      media.currentTime = Math.min(
        pendingResumeMsRef.current / 1000,
        Math.max(0, nextDuration - 0.25),
      );
      setCurrentTime(media.currentTime);
    }
    pendingResumeMsRef.current = 0;

    if (autoplayNextRef.current) {
      autoplayNextRef.current = false;
      void media.play().catch(() => undefined);
    }

    void invoke("update_media_duration", {
      id: current.id,
      durationMs: Math.round(nextDuration * 1000),
    });
  }, [current, playbackRate, volume]);

  const onEnded = useCallback(() => {
    const media = mediaRef.current;
    if (media) media.currentTime = 0;
    setCurrentTime(0);
    void persistPlayback(true);

    // Continuous play: next item in library order, or wrap around the list.
    if (continueMode !== "off" && current && items.length > 0) {
      const index = items.findIndex((item) => item.id === current.id);
      const nextItem =
        index >= 0 && index + 1 < items.length
          ? items[index + 1]
          : continueMode === "loop-list"
            ? items[0]
            : null;
      if (nextItem?.id === current.id) {
        void media?.play().catch(() => undefined);
        return;
      }
      if (nextItem) {
        autoplayNextRef.current = true;
        void choose({ ...nextItem, playback_position: 0 });
        return;
      }
    }
    setPlaying(false);
  }, [choose, continueMode, current, items, persistPlayback]);

  const beginResize = useCallback(
    (side: "sidebar" | "inspector", event: ReactPointerEvent<HTMLDivElement>) => {
      event.preventDefault();
      const startX = event.clientX;
      const startWidth = side === "sidebar" ? sidebarWidth : inspectorWidth;
      document.body.classList.add("is-resizing");

      const move = (moveEvent: PointerEvent) => {
        const delta = moveEvent.clientX - startX;
        if (side === "sidebar") {
          setSidebarWidth(clamp(startWidth + delta, 190, 380));
        } else {
          setInspectorWidth(clamp(startWidth - delta, 250, 460));
        }
      };

      const up = () => {
        window.removeEventListener("pointermove", move);
        window.removeEventListener("pointerup", up);
        document.body.classList.remove("is-resizing");
      };

      window.addEventListener("pointermove", move);
      window.addEventListener("pointerup", up);
    },
    [inspectorWidth, sidebarWidth],
  );

  useEffect(() => {
    document.documentElement.dataset.theme = resolvedTheme;
  }, [resolvedTheme]);

  const refreshDubPlan = useCallback(() => {
    if (!current || activeRail !== "studio") {
      setDubPlan(null);
      return;
    }
    invoke<DubPlan>("plan_dub", { mediaId: current.id })
      .then(setDubPlan)
      .catch(() => setDubPlan(null));
  }, [activeRail, current]);

  // The dub plan is derived from Segments + TTS assets; recompute on change.
  useEffect(() => {
    refreshDubPlan();
  }, [refreshDubPlan, segments]);

  const dubPlacements = useMemo(
    () => new Map((dubPlan?.placements ?? []).map((row) => [row.segment_id, row])),
    [dubPlan],
  );

  useEffect(() => {
    const onChange = (event: MediaQueryListEvent) => setSystemDark(event.matches);
    prefersDark.addEventListener("change", onChange);
    return () => prefersDark.removeEventListener("change", onChange);
  }, [prefersDark]);

  useEffect(() => {
    const payload: UiState = {
      activeRail,
      sidebarWidth,
      inspectorWidth,
      sidebarVisible,
      inspectorVisible,
      openTabIds,
      activeMediaId: current?.id ?? null,
      theme,
    };
    localStorage.setItem(UI_KEY, JSON.stringify(payload));
  }, [
    activeRail,
    current?.id,
    inspectorVisible,
    inspectorWidth,
    openTabIds,
    sidebarVisible,
    sidebarWidth,
    theme,
  ]);

  useEffect(() => {
    const media = mediaRef.current;
    if (media && sourceUrl) {
      media.load();
    }
  }, [sourceUrl]);

  useEffect(() => {
    void (async () => {
      const mediaItems = await refreshLibrary();
      const restored = mediaItems.find(
        (item) => item.id === initial.activeMediaId,
      );
      if (restored) {
        await choose(restored, false);
        return;
      }
      const firstOpen = initial.openTabIds
        ?.map((id) => mediaItems.find((item) => item.id === id))
        .find((item): item is MediaItem => Boolean(item));
      if (firstOpen) await choose(firstOpen, false);
    })();
  }, [choose, initial.activeMediaId, initial.openTabIds, refreshLibrary]);

  // P5: every shortcut goes through the user's keymap (Settings → Shortcuts).
  const [keymap, setKeymap] = useState<Keymap>(loadKeymap);
  const [shortcutError, setShortcutError] = useState<string | null>(null);
  useEffect(() => saveKeymap(keymap), [keymap]);

  const runAction = useCallback(
    (action: LocalAction) => {
      switch (action) {
        case "togglePlay":
          void togglePlayback();
          break;
        case "seekBack":
          seekBy(-5);
          break;
        case "seekForward":
          seekBy(5);
          break;
        case "seekBackLong":
          seekBy(-15);
          break;
        case "seekForwardLong":
          seekBy(15);
          break;
        case "prevSegment":
          stepSegment(-1);
          break;
        case "nextSegment":
          stepSegment(1);
          break;
        case "sentenceLoop":
          toggleSentenceLoop();
          break;
        case "loopA":
          setLoopPoint("a");
          break;
        case "loopB":
          setLoopPoint("b");
          break;
        case "clearLoop":
          clearLoop();
          break;
        case "revealHold":
          setRevealHeld(true);
          break;
        case "toggleBlind":
          setBlindMode((value) => !value);
          break;
        case "toggleAutoPause":
          setAutoPause((value) => !value);
          break;
        case "cycleContinue":
          setContinueMode((value) =>
            value === "off" ? "next" : value === "next" ? "loop-list" : "off",
          );
          break;
      }
    },
    [clearLoop, seekBy, setLoopPoint, stepSegment, togglePlayback, toggleSentenceLoop],
  );
  const runActionRef = useRef(runAction);
  useEffect(() => {
    runActionRef.current = runAction;
  }, [runAction]);

  useEffect(() => {
    const repeatable: Partial<Record<LocalAction, true>> = {
      seekBack: true,
      seekForward: true,
      seekBackLong: true,
      seekForwardLong: true,
      revealHold: true,
    };
    const onKeydown = (event: KeyboardEvent) => {
      const mod = event.metaKey || event.ctrlKey;

      if (mod && !event.altKey && event.key.toLowerCase() === "b") {
        event.preventDefault();
        if (event.shiftKey) {
          setInspectorVisible((value) => !value);
        } else {
          setSidebarVisible((value) => !value);
        }
        return;
      }

      if (isEditableTarget(event.target)) return;
      if (event.target instanceof Element && event.target.closest("[data-shortcut-capture]")) return;
      const combo = comboFromEvent(event);
      if (!combo) return;
      const action = matchLocal(keymap, combo);
      if (!action) return;
      event.preventDefault();
      if (event.repeat && !repeatable[action]) return;
      runActionRef.current(action);
    };
    const onKeyup = (event: KeyboardEvent) => {
      if (keyFromCode(event.code) === comboKey(keymap.local.revealHold)) setRevealHeld(false);
    };
    const release = () => setRevealHeld(false);

    window.addEventListener("keydown", onKeydown);
    window.addEventListener("keyup", onKeyup);
    window.addEventListener("blur", release);
    return () => {
      window.removeEventListener("keydown", onKeydown);
      window.removeEventListener("keyup", onKeyup);
      window.removeEventListener("blur", release);
    };
  }, [keymap]);

  // System-wide hotkeys are opt-in so YUZUKI never steals keys by default.
  useEffect(() => {
    if (!keymap.globalEnabled) {
      setShortcutError(null);
      return;
    }
    let cancelled = false;
    let done = false;
    const registered: string[] = [];
    void (async () => {
      const failed: string[] = [];
      for (const { id } of GLOBAL_ACTIONS) {
        const accelerator = keymap.global[id];
        if (!accelerator || cancelled) continue;
        try {
          await registerGlobalShortcut(accelerator, (event) => {
            if (event.state === "Pressed") runActionRef.current(id);
          });
          registered.push(accelerator);
        } catch {
          failed.push(accelerator);
        }
      }
      done = true;
      if (cancelled) {
        await unregisterGlobalShortcut(registered).catch(() => undefined);
        return;
      }
      setShortcutError(
        failed.length
          ? "Could not register " + failed.join(", ") + " (already used by another app or invalid)."
          : null,
      );
    })();
    return () => {
      cancelled = true;
      if (done) void unregisterGlobalShortcut(registered).catch(() => undefined);
    };
  }, [keymap.global, keymap.globalEnabled]);

  useEffect(
    () => () => {
      void persistPlayback(true);
    },
    [persistPlayback],
  );

  const cycleTheme = () => {
    setTheme((value) =>
      value === "system" ? "dark" : value === "dark" ? "light" : "system",
    );
  };

  const attachMedia = useCallback((node: HTMLMediaElement | null) => {
    mediaRef.current = node;
    setMediaElement(node);
  }, []);

  const mediaElementView = current ? (
    current.media_type === "video" ? (
      <video
        ref={attachMedia}
        src={sourceUrl}
        className="video-stage"
        playsInline
        onTimeUpdate={onTimeUpdate}
        onLoadedMetadata={onLoadedMetadata}
        onPlay={() => setPlaying(true)}
        onPause={() => {
          setPlaying(false);
          void persistPlayback(true);
        }}
        onEnded={onEnded}
      />
    ) : (
      <audio
        ref={attachMedia}
        src={sourceUrl}
        className="audio-engine"
        onTimeUpdate={onTimeUpdate}
        onLoadedMetadata={onLoadedMetadata}
        onPlay={() => setPlaying(true)}
        onPause={() => {
          setPlaying(false);
          void persistPlayback(true);
        }}
        onEnded={onEnded}
      />
    )
  ) : null;

  return (
    <div
      className={[
        "workbench",
        sidebarVisible ? "" : "sidebar-hidden",
        inspectorVisible && activeRail !== "settings" ? "" : "inspector-hidden",
      ]
        .filter(Boolean)
        .join(" ")}
      style={workbenchStyle}
    >
      <aside className="rail">
        <button className="brand" aria-label="YUZUKI">Y</button>
        <nav>
          {RAILS.map((item) => (
            <button
              key={item}
              className={activeRail === item ? "active" : ""}
              onClick={() => setActiveRail(item)}
              title={item}
            >
              <span>{RAIL_ICONS[item]}</span>
            </button>
          ))}
        </nav>
        <button
          className="rail-bottom"
          title={`Theme: ${theme}`}
          onClick={cycleTheme}
        >
          {themeIcon}
        </button>
      </aside>

      <aside className="sidebar">
        <div
          className="resize-handle resize-handle-right"
          onPointerDown={(event) => beginResize("sidebar", event)}
        />
        <div className="sidebar-head">
          <div>
            <p className="eyebrow">{activeRail.toUpperCase()}</p>
            <h1>YUZUKI</h1>
          </div>
          <button className="icon-button" onClick={() => void importFiles()} title="Import media">
            ＋
          </button>
        </div>

        {activeRail === "library" || activeRail === "studio" ? (
          <>
            <div className="source-group">
              <p className="section-label">MEDIA</p>
              <div className="library-tools">
                <input
                  type="search"
                  placeholder="Search library"
                  value={libraryQuery}
                  onChange={(event) => setLibraryQuery(event.target.value)}
                />
                <select
                  value={librarySort}
                  title="Sort library"
                  onChange={(event) => setLibrarySort(event.target.value as typeof librarySort)}
                >
                  <option value="added">Newest</option>
                  <option value="title">Title</option>
                  <option value="duration">Duration</option>
                </select>
              </div>
              {visibleItems.map((item) => (
                <button
                  key={item.id}
                  className={`media-row ${current?.id === item.id ? "selected" : ""}`}
                  onClick={() => void choose(item)}
                >
                  <span className="media-glyph">
                    {item.media_type === "video" ? "▤" : "♪"}
                  </span>
                  <span className="media-copy">
                    <strong>{item.title}</strong>
                    <small>
                      {item.duration_ms
                        ? formatTime(item.duration_ms / 1000)
                        : "Local media"}
                    </small>
                  </span>
                </button>
              ))}
              {!items.length && (
                <button className="empty-import" onClick={() => void importFiles()}>
                  Import your first audio or video
                </button>
              )}
            </div>
            <div className="source-group">
              <p className="section-label">SOURCES</p>
              {sourceList.map((source) => (
                <div key={source.id} className="source-row">
                  <span>◉</span>
                  <span>{source.label}</span>
                </div>
              ))}
              <button className="source-row" onClick={() => void importFolder()}>
                <span>⌁</span>
                <span>Import folder…</span>
              </button>
            </div>
          </>
        ) : (
          <>
            {activeRail === "settings" ? (
              <nav className="source-group settings-nav" aria-label="Settings sections">
                {SETTINGS_TABS.map((tab) => (
                  <button
                    key={tab.id}
                    className={"settings-nav-row" + (settingsTab === tab.id ? " active" : "")}
                    aria-current={settingsTab === tab.id ? "page" : undefined}
                    onClick={() => setSettingsTab(tab.id)}
                  >
                    {tab.label}
                  </button>
                ))}
              </nav>
            ) : (
              <div className="source-group shell-placeholder"> <p className="section-label">SECTION</p> <strong>{activeRail}</strong> <small>Not in this beta yet. Your library and playback stay available.</small> </div>
            )}
          </>
        )}
      </aside>

      <main className="main">
        <header className="tabs">
          <button
            className="chrome-button"
            title="Toggle sidebar · Ctrl/Cmd+B"
            onClick={() => setSidebarVisible((value) => !value)}
          >
            ☰
          </button>
          <div className="tab-scroll">
            {openTabs.map((tab) => (
              <button
                key={tab.id}
                className={`tab ${current?.id === tab.id ? "active" : ""}`}
                onClick={() => void choose(tab, false)}
              >
                <span>{tab.title}</span>
                <span
                  className="tab-close"
                  title="Close tab"
                  onClick={(event) => {
                    event.stopPropagation();
                    void closeTab(tab.id);
                  }}
                >
                  ×
                </span>
              </button>
            ))}
            {!openTabs.length && activeRail !== "settings" && <div className="tab active">Library</div>}
          </div>
          <button className="tab-add" title="Import media" onClick={() => void importFiles()}>
            ＋
          </button>
          <button
            className="chrome-button"
            title="Toggle inspector · Ctrl/Cmd+Shift+B"
            onClick={() => setInspectorVisible((value) => !value)}
          >
            ◫
          </button>
        </header>

        {activeRail === "settings" ? (
          <div className="content settings-stack" style={{ overflowY: "auto", minHeight: 0 }}>
            {settingsTab === "providers" && <ProviderSettings />}
            {settingsTab === "voices" && <VoiceProfileSettings />}
            {settingsTab === "shortcuts" && (
              <ShortcutSettings keymap={keymap} onChange={setKeymap} error={shortcutError} />
            )}
            {settingsTab === "dictionary" && <DictionaryPanel />}
            {settingsTab === "updates" && <DesktopUpdater visible />}
          </div>
        ) : mediaWorkspaceVisible ? (
          <section className="content media-workspace">
            <div className={`hero ${current?.media_type === "video" ? "video-hero" : ""}`}>
              {(!current || current.media_type === "audio") && (
                <div className="artwork">
                  <span>{current ? "♪" : "T"}</span>
                </div>
              )}

              {mediaElementView}

              <div className="hero-copy">
                <p className="eyebrow">
                  {current ? "NOW WORKING" : "LOCAL-FIRST ASMR WORKBENCH"}
                </p>
                <h2>
                  {current?.title || "Your library, transcript and dub in one place."}
                </h2>
                <p>
                  {current
                    ? "Original media · sentence timeline · modular processing"
                    : "Import media to start. Provider-backed ASR and dubbing arrive through capability contracts."}
                </p>
                <div className="hero-actions">
                  {!current ? (
                    <button className="primary" onClick={() => void importFiles()}>
                      Import Media
                    </button>
                  ) : (
                    <>
                      <button className="primary" onClick={() => void togglePlayback()}>
                        {playing ? "Pause" : "Play"}
                      </button>
                      <button className="secondary" onClick={() => setActiveRail("studio")}>
                        Open Studio
                      </button>
                    </>
                  )}
                </div>
              </div>
            </div>

            {current && (
              <>
                {activeRail === "studio" && (
                  <>
                    <AsrPanel
                      media={current}
                      onSegments={(rows) => {
                        setSegments(rows);
                        setSelected(null);
                      }}
                    />
                    <RefinePanel
                      media={current}
                      segments={segments}
                      onSegmentUpdated={handleSegmentUpdated}
                      onSelectSegment={(segmentId) => {
                        const target = segments.find((row) => row.id === segmentId);
                        if (target) chooseSegment(target);
                      }}
                    />
                    <TranslatePanel
                      media={current}
                      segments={segments}
                      onSegments={(rows) => {
                        setSegments(rows);
                        setSelected((currentSelection) =>
                          currentSelection
                            ? (rows.find((row) => row.id === currentSelection.id) ?? null)
                            : null,
                        );
                      }}
                    />
                    <DubExportActions
                      media={current}
                      plan={dubPlan}
                      onRefreshPlan={refreshDubPlan}
                    />
                  </>
                )}

                <div className="timeline-head">
                  <div>
                    <p className="section-label">TIMELINE</p>
                    <strong>{segments.length} segments</strong>
                  </div>
                  <div className="timeline-actions">
                    <button
                      className={sentenceLoop ? "active" : ""}
                      onClick={toggleSentenceLoop}
                    >
                      Sentence Loop
                    </button>
                    <button
                      className={loopA !== null ? "active" : ""}
                      onClick={() => setLoopPoint("a")}
                    >
                      A {loopA === null ? "" : formatTime(loopA)}
                    </button>
                    <button
                      className={loopB !== null ? "active" : ""}
                      onClick={() => setLoopPoint("b")}
                    >
                      B {loopB === null ? "" : formatTime(loopB)}
                    </button>
                    {(loopA !== null || loopB !== null || sentenceLoop) && (
                      <button onClick={clearLoop}>Clear</button>
                    )}
                    <button
                      className={autoPause ? "active" : ""}
                      title="Pause at the end of every sentence · P"
                      onClick={() => setAutoPause((value) => !value)}
                    >
                      Auto-pause
                    </button>
                    <button
                      className={blindMode ? "active" : ""}
                      title="Blind listening: hide translations, hold H to peek · Shift+H"
                      onClick={() => setBlindMode((value) => !value)}
                    >
                      Blind
                    </button>
                    <button
                      className={continueMode !== "off" ? "active" : ""}
                      title="When a track ends: stop, play the next one, or loop the list · N"
                      onClick={() =>
                        setContinueMode((value) =>
                          value === "off" ? "next" : value === "next" ? "loop-list" : "off",
                        )
                      }
                    >
                      {continueMode === "off"
                        ? "Stop at end"
                        : continueMode === "next"
                          ? "Play next"
                          : "Loop list"}
                    </button>
                  </div>
                </div>

                <SubtitleActions
                  media={current}
                  segmentCount={segments.length}
                  onImported={handleSubtitleImported}
                />

                <WaveformEditor
                  media={mediaElement}
                  sourceUrl={sourceUrl}
                  segments={segments}
                  selectedId={selected?.id ?? null}
                  onSelect={chooseSegment}
                  onTimingCommit={commitSegmentTiming}
                />

                {dictPopup && (
                  <div className="dict-popup" style={{ position: "fixed", left: dictPopup.x + 8, top: dictPopup.y + 8, zIndex: 50 }}>
                    <div className="dict-popup-head">
                      <strong>{dictPopup.query}</strong>
                      <button type="button" aria-label="Close dictionary" onClick={() => setDictPopup(null)}>×</button>
                    </div>
                    <DictResults result={dictPopup.result} />
                  </div>
                )}
                <div className="segments" title="Select a word to look it up" onMouseUp={(event) => void lookupSelection(event)}>
                  {segments.map((segment) => (
                    <button
                      key={segment.id}
                      className={[
                        "segment",
                        selected?.id === segment.id ? "active" : "",
                        activeSegment?.id === segment.id ? "playing" : "",
                      ]
                        .filter(Boolean)
                        .join(" ")}
                      onClick={() => chooseSegment(segment)}
                    >
                      <span className="time">{formatTime(segment.start_ms / 1000)}</span>
                      <span className="segment-copy">
                        <strong>{segment.source_text || "Untitled segment"}</strong>
                        <small>
                          {!segment.translated_text
                            ? "No translation yet"
                            : translationHidden
                              ? "Hold H to show translation"
                              : segment.translated_text}
                        </small>
                        {segment.dub_pushed_ms > 0 && (
                          <span
                            className="state-badge pushed-badge"
                            title={`Dub ${pushedLabel(segment.dub_pushed_ms)} after regenerating an earlier sentence. Preview it or press OK in the Inspector to clear.`}
                          >
                            Pushed +{(segment.dub_pushed_ms / 1000).toFixed(1)}s
                          </span>
                        )}
                        {dubPlacements.get(segment.id)?.flags.includes("drift_exceeded") && (
                          <span
                            className="state-badge drift-badge"
                            title="Dub starts too late after this sentence. Shorten the translation or skip this sentence's dub."
                          >
                            Dub drift +
                            {(
                              (dubPlacements.get(segment.id)?.drift_ms ?? 0) / 1000
                            ).toFixed(1)}
                            s
                          </span>
                        )}
                      </span>
                    </button>
                  ))}
                  {!segments.length && (
                    <div className="empty-state">
                      <strong>No sentence timeline yet</strong>
                      <span>
                        ASR providers and subtitle import will populate canonical Segments.
                      </span>
                    </div>
                  )}
                </div>
              </>
            )}
          </section>
        ) : activeRail === "jobs" ? (
          <JobsView media={items} />
        ) : (
          <section className="content section-placeholder">
            {activeRail === "home" && (
              <GetStarted
                progress={getStartedProgress}
                onOpen={(step) =>
                  step === "import"
                    ? void importFiles()
                    : setActiveRail(step.endsWith("Provider") ? "settings" : "studio")
                }
              />
            )}
            <p className="eyebrow">{activeRail.toUpperCase()}</p>
            <h2>
              {activeRail === "home"
                ? "Continue where you left off."
                : `${activeRail[0].toUpperCase()}${activeRail.slice(1)} isn't available in this beta yet.`}
            </h2>
            <p>
              The global player stays alive while product surfaces change. This is the
              Workbench contract, not a page-based player.
            </p>
            {current && (
              <button className="secondary" onClick={() => setActiveRail("library")}>
                Return to {current.title}
              </button>
            )}
          </section>
        )}
      </main>

      <aside className="inspector">
        <div
          className="resize-handle resize-handle-left"
          onPointerDown={(event) => beginResize("inspector", event)}
        />
        {selected ? (
          <SegmentInspector
            segment={selected}
            onUpdated={handleSegmentUpdated}
            onSegmentsChanged={refreshSegmentsById}
          />
        ) : (
          <>
            <p className="eyebrow">INSPECTOR</p>
            <h3>{current ? "Track" : "Nothing selected"}</h3>
            <p className="inspector-help">
              {current
                ? "Select a sentence to edit transcript, translation and later voice settings."
                : "Select media or a sentence to inspect it here."}
            </p>
            {current && (
              <div className="track-facts">
                <div><span>Type</span><strong>{current.media_type}</strong></div>
                <div><span>Duration</span><strong>{formatTime(duration)}</strong></div>
                <div><span>Speed</span><strong>{playbackRate.toFixed(2)}×</strong></div>
                <div><span>Volume</span><strong>{Math.round(volume * 100)}%</strong></div>
              </div>
            )}
          </>
        )}
      </aside>

      <footer className="playerbar">
        <div className="now-playing">
          <div className="mini-art">{current?.media_type === "video" ? "▤" : "♪"}</div>
          <div>
            <strong>{current?.title || "Nothing Playing"}</strong>
            <small>
              {sentenceLoop
                ? "Sentence loop"
                : loopA !== null || loopB !== null
                  ? "A–B loop"
                  : "Original"}
            </small>
          </div>
        </div>

        <div className="transport">
          <div className="transport-buttons">
            <button title="Previous sentence · [" onClick={() => stepSegment(-1)}>‹|</button>
            <button title="Back 15 seconds" onClick={() => seekBy(-15)}>−15</button>
            <button className="play" title="Play/Pause · Space" onClick={() => void togglePlayback()}>
              {playing ? "Ⅱ" : "▶"}
            </button>
            <button title="Forward 15 seconds" onClick={() => seekBy(15)}>+15</button>
            <button title="Next sentence · ]" onClick={() => stepSegment(1)}>|›</button>
          </div>
          <div className="scrubber">
            <span>{formatTime(currentTime)}</span>
            <input
              type="range"
              min="0"
              max={duration || 1}
              step="0.1"
              value={currentTime}
              onChange={(event) => seekTo(Number(event.target.value))}
            />
            <span>{formatTime(duration)}</span>
          </div>
        </div>

        <div className="player-meta">
          <select value={playbackRate} title="Playback speed" onChange={onRate}>
            {[0.75, 1, 1.25, 1.5, 2].map((rate) => (
              <option key={rate} value={rate}>{rate}×</option>
            ))}
          </select>
          <label className="volume-control" title="Volume">
            <span>◖</span>
            <input
              type="range"
              min="0"
              max="1"
              step="0.02"
              value={volume}
              onChange={onVolume}
            />
          </label>
          <button
            className={miniOpen ? "active" : ""}
            title="Floating subtitle window (always on top)"
            onClick={() => void toggleMini()}
          >
            {miniOpen ? "Close subtitles" : "Float subtitles"}
          </button>
          {miniOpen && miniLocked && (
            <button title="The floating subtitles are click-through; unlock to move them" onClick={unlockMini}>
              Unlock
            </button>
          )}
        </div>
      </footer>
    </div>
  );
}
