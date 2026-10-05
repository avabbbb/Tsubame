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
import { open } from "@tauri-apps/plugin-dialog";
import type {
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
];

const RAIL_ICONS: Record<RailSection, string> = {
  home: "⌂",
  library: "▣",
  discover: "⌕",
  downloads: "↓",
  studio: "✦",
  jobs: "≡",
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
  const [currentTime, setCurrentTime] = useState(0);
  const [duration, setDuration] = useState(0);
  const [playing, setPlaying] = useState(false);
  const [playbackRate, setPlaybackRate] = useState(1);
  const [volume, setVolume] = useState(1);
  const [draftSource, setDraftSource] = useState("");
  const [draftTranslation, setDraftTranslation] = useState("");
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
  const [loopB, setLoopB] = useState<number | null>(null);

  const mediaRef = useRef<HTMLMediaElement | null>(null);
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

  const activeSegment = useMemo(() => {
    const nowMs = currentTime * 1000;
    return (
      segments.find(
        (segment) => nowMs >= segment.start_ms && nowMs < segment.end_ms,
      ) ?? null
    );
  }, [currentTime, segments]);

  const resolvedTheme =
    theme === "system" ? (systemDark ? "dark" : "light") : theme;

  const themeIcon =
    theme === "system" ? "◐" : theme === "dark" ? "☾" : "☀";

  const workbenchStyle = {
    "--sidebar-width": sidebarVisible ? `${sidebarWidth}px` : "0px",
    "--inspector-width": inspectorVisible
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
      setDraftSource(segment.source_text);
      setDraftTranslation(segment.translated_text);
      seekTo(segment.start_ms / 1000);
    },
    [seekTo],
  );

  const saveSegment = useCallback(async () => {
    if (!selected) return;

    const updated = await invoke<Segment>("update_segment", {
      id: selected.id,
      expectedRevision: selected.revision,
      sourceText: draftSource,
      translatedText: draftTranslation,
      startMs: selected.start_ms,
      endMs: selected.end_ms,
    });

    setSegments((rows) =>
      rows.map((row) => (row.id === updated.id ? updated : row)),
    );
    setSelected(updated);
  }, [draftSource, draftTranslation, selected]);

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

    void persistPlayback();
  }, [loopA, loopB, persistPlayback, segments, sentenceLoop]);

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

    void invoke("update_media_duration", {
      id: current.id,
      durationMs: Math.round(nextDuration * 1000),
    });
  }, [current, playbackRate, volume]);

  const onEnded = useCallback(() => {
    setPlaying(false);
    setCurrentTime(0);
    if (mediaRef.current) mediaRef.current.currentTime = 0;
    void persistPlayback(true);
  }, [persistPlayback]);

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

  useEffect(() => {
    const onKeydown = (event: KeyboardEvent) => {
      const mod = event.metaKey || event.ctrlKey;

      if (mod && event.key.toLowerCase() === "b") {
        event.preventDefault();
        if (event.shiftKey) {
          setInspectorVisible((value) => !value);
        } else {
          setSidebarVisible((value) => !value);
        }
        return;
      }

      if (isEditableTarget(event.target)) return;

      if (event.code === "Space") {
        event.preventDefault();
        void togglePlayback();
      } else if (event.key === "ArrowLeft") {
        event.preventDefault();
        seekBy(event.shiftKey ? -15 : -5);
      } else if (event.key === "ArrowRight") {
        event.preventDefault();
        seekBy(event.shiftKey ? 15 : 5);
      } else if (event.key === "[") {
        event.preventDefault();
        stepSegment(-1);
      } else if (event.key === "]") {
        event.preventDefault();
        stepSegment(1);
      } else if (event.key.toLowerCase() === "l" && !event.shiftKey) {
        event.preventDefault();
        toggleSentenceLoop();
      } else if (event.shiftKey && event.key.toLowerCase() === "a") {
        event.preventDefault();
        setLoopPoint("a");
      } else if (event.shiftKey && event.key.toLowerCase() === "b") {
        event.preventDefault();
        setLoopPoint("b");
      } else if (event.key === "Escape") {
        clearLoop();
      }
    };

    window.addEventListener("keydown", onKeydown);
    return () => window.removeEventListener("keydown", onKeydown);
  }, [
    clearLoop,
    seekBy,
    setLoopPoint,
    stepSegment,
    togglePlayback,
    toggleSentenceLoop,
  ]);

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

  const mediaElement = current ? (
    current.media_type === "video" ? (
      <video
        ref={(node) => {
          mediaRef.current = node;
        }}
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
        ref={(node) => {
          mediaRef.current = node;
        }}
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
        inspectorVisible ? "" : "inspector-hidden",
      ]
        .filter(Boolean)
        .join(" ")}
      style={workbenchStyle}
    >
      <aside className="rail">
        <button className="brand" aria-label="Tsubame">T</button>
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
            <h1>Tsubame</h1>
          </div>
          <button className="icon-button" onClick={() => void importFiles()} title="Import media">
            ＋
          </button>
        </div>

        {activeRail === "library" || activeRail === "studio" ? (
          <>
            <div className="source-group">
              <p className="section-label">MEDIA</p>
              {items.map((item) => (
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
              <div className="source-row"><span>◉</span><span>Japanese ASMR</span></div>
              <div className="source-row muted"><span>○</span><span>ASMR.one</span></div>
              <div className="source-row muted"><span>⌁</span><span>Local folders</span></div>
            </div>
          </>
        ) : (
          <div className="source-group shell-placeholder">
            <p className="section-label">SECTION</p>
            <strong>{activeRail}</strong>
            <small>This surface is reserved; playback remains global.</small>
          </div>
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
            {!openTabs.length && <div className="tab active">Library</div>}
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

        {mediaWorkspaceVisible ? (
          <section className="content media-workspace">
            <div className={`hero ${current?.media_type === "video" ? "video-hero" : ""}`}>
              {(!current || current.media_type === "audio") && (
                <div className="artwork">
                  <span>{current ? "♪" : "T"}</span>
                </div>
              )}

              {mediaElement}

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
                  </div>
                </div>

                <div className="segments">
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
                        <small>{segment.translated_text || "No translation yet"}</small>
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
        ) : (
          <section className="content section-placeholder">
            <p className="eyebrow">{activeRail.toUpperCase()}</p>
            <h2>
              {activeRail === "home"
                ? "Continue where you left off."
                : `${activeRail} is ready for its next adapter.`}
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
          <>
            <p className="eyebrow">SEGMENT {selected.ordinal + 1}</p>
            <h3>Sentence Inspector</h3>
            <label>
              Original
              <textarea
                value={draftSource}
                onChange={(event) => setDraftSource(event.target.value)}
              />
            </label>
            <label>
              Translation
              <textarea
                value={draftTranslation}
                onChange={(event) => setDraftTranslation(event.target.value)}
              />
            </label>
            <div className="time-grid">
              <label>
                Start
                <input value={formatTime(selected.start_ms / 1000)} readOnly />
              </label>
              <label>
                End
                <input value={formatTime(selected.end_ms / 1000)} readOnly />
              </label>
            </div>
            <button className="primary wide" onClick={() => void saveSegment()}>
              Save changes
            </button>
            <button className="secondary wide" disabled>
              Regenerate Segment · planned
            </button>
          </>
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
        </div>
      </footer>
    </div>
  );
}
