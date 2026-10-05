<script setup lang="ts">
import {
  computed,
  nextTick,
  onBeforeUnmount,
  onMounted,
  ref,
  watch,
} from "vue";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type {
  MediaItem,
  RailSection,
  Segment,
  ThemePreference,
} from "./types";

const UI_KEY = "tsubame-workbench-v1";
const RAILS: RailSection[] = [
  "home",
  "library",
  "discover",
  "downloads",
  "studio",
  "jobs",
];

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

const initial = readUiState();
const items = ref<MediaItem[]>([]);
const current = ref<MediaItem | null>(null);
const segments = ref<Segment[]>([]);
const selected = ref<Segment | null>(null);
const media = ref<HTMLMediaElement | null>(null);
const currentTime = ref(0);
const duration = ref(0);
const playing = ref(false);
const playbackRate = ref(1);
const volume = ref(1);
const draftSource = ref("");
const draftTranslation = ref("");
const activeRail = ref<RailSection>(initial.activeRail ?? "library");
const openTabIds = ref<number[]>(initial.openTabIds ?? []);
const sidebarWidth = ref(initial.sidebarWidth ?? 238);
const inspectorWidth = ref(initial.inspectorWidth ?? 310);
const sidebarVisible = ref(initial.sidebarVisible ?? true);
const inspectorVisible = ref(initial.inspectorVisible ?? true);
const theme = ref<ThemePreference>(initial.theme ?? "system");
const sentenceLoop = ref(false);
const loopA = ref<number | null>(null);
const loopB = ref<number | null>(null);
const pendingResumeMs = ref(0);
let lastPersistAt = 0;

const prefersDark = window.matchMedia("(prefers-color-scheme: dark)");

const sourceUrl = computed(() =>
  current.value ? convertFileSrc(current.value.path) : "",
);
const openTabs = computed(() =>
  openTabIds.value
    .map((id) => items.value.find((item) => item.id === id))
    .filter((item): item is MediaItem => !!item),
);
const mediaWorkspaceVisible = computed(
  () => activeRail.value === "library" || activeRail.value === "studio",
);
const activeSegment = computed(() => {
  const ms = currentTime.value * 1000;
  return segments.value.find(
    (segment) => ms >= segment.start_ms && ms < segment.end_ms,
  ) ?? null;
});
const resolvedTheme = computed(() =>
  theme.value === "system" ? (prefersDark.matches ? "dark" : "light") : theme.value,
);
const themeIcon = computed(() =>
  theme.value === "system" ? "◐" : theme.value === "dark" ? "☾" : "☀",
);
const workbenchStyle = computed(() => ({
  "--sidebar-width": sidebarVisible.value ? `${sidebarWidth.value}px` : "0px",
  "--inspector-width": inspectorVisible.value
    ? `${inspectorWidth.value}px`
    : "0px",
}));

function persistUiState() {
  const payload: UiState = {
    activeRail: activeRail.value,
    sidebarWidth: sidebarWidth.value,
    inspectorWidth: inspectorWidth.value,
    sidebarVisible: sidebarVisible.value,
    inspectorVisible: inspectorVisible.value,
    openTabIds: openTabIds.value,
    activeMediaId: current.value?.id ?? null,
    theme: theme.value,
  };
  localStorage.setItem(UI_KEY, JSON.stringify(payload));
}

function applyTheme() {
  document.documentElement.dataset.theme = resolvedTheme.value;
}

function cycleTheme() {
  theme.value =
    theme.value === "system"
      ? "dark"
      : theme.value === "dark"
        ? "light"
        : "system";
  applyTheme();
  persistUiState();
}

function setRail(section: RailSection) {
  activeRail.value = section;
  persistUiState();
}

function clamp(value: number, min: number, max: number) {
  return Math.max(min, Math.min(max, value));
}

function beginResize(side: "sidebar" | "inspector", event: PointerEvent) {
  event.preventDefault();
  const startX = event.clientX;
  const startWidth =
    side === "sidebar" ? sidebarWidth.value : inspectorWidth.value;
  document.body.classList.add("is-resizing");

  const move = (moveEvent: PointerEvent) => {
    const delta = moveEvent.clientX - startX;
    if (side === "sidebar") {
      sidebarWidth.value = clamp(startWidth + delta, 190, 380);
    } else {
      inspectorWidth.value = clamp(startWidth - delta, 250, 460);
    }
  };
  const up = () => {
    window.removeEventListener("pointermove", move);
    window.removeEventListener("pointerup", up);
    document.body.classList.remove("is-resizing");
    persistUiState();
  };
  window.addEventListener("pointermove", move);
  window.addEventListener("pointerup", up);
}

function toggleSidebar() {
  sidebarVisible.value = !sidebarVisible.value;
  persistUiState();
}

function toggleInspector() {
  inspectorVisible.value = !inspectorVisible.value;
  persistUiState();
}

async function refreshLibrary() {
  items.value = await invoke<MediaItem[]>("list_media");
  openTabIds.value = openTabIds.value.filter((id) =>
    items.value.some((item) => item.id === id),
  );
}

async function importFiles() {
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
  await refreshLibrary();
  const newest = items.value[0];
  if (newest) await choose(newest);
}

async function choose(item: MediaItem, addTab = true) {
  if (current.value?.id !== item.id) await persistPlayback(true);
  current.value = item;
  selected.value = null;
  playbackRate.value = item.speed || 1;
  volume.value = Number.isFinite(item.volume) ? item.volume : 1;
  pendingResumeMs.value = item.playback_position;
  currentTime.value = item.playback_position / 1000;
  duration.value = item.duration_ms / 1000;
  segments.value = await invoke<Segment[]>("get_segments", { mediaId: item.id });

  if (addTab && !openTabIds.value.includes(item.id)) {
    openTabIds.value.push(item.id);
  }
  persistUiState();
  await nextTick();
  media.value?.load();
}

async function closeTab(id: number) {
  const index = openTabIds.value.indexOf(id);
  if (index < 0) return;
  openTabIds.value.splice(index, 1);
  if (current.value?.id === id) {
    await persistPlayback(true);
    const fallbackId =
      openTabIds.value[Math.min(index, openTabIds.value.length - 1)] ?? null;
    const fallback = items.value.find((item) => item.id === fallbackId);
    if (fallback) await choose(fallback, false);
    else {
      current.value = null;
      segments.value = [];
      selected.value = null;
      playing.value = false;
    }
  }
  persistUiState();
}

function chooseSegment(segment: Segment) {
  selected.value = segment;
  draftSource.value = segment.source_text;
  draftTranslation.value = segment.translated_text;
  seekTo(segment.start_ms / 1000);
}

async function saveSegment() {
  if (!selected.value) return;
  const updated = await invoke<Segment>("update_segment", {
    id: selected.value.id,
    expectedRevision: selected.value.revision,
    sourceText: draftSource.value,
    translatedText: draftTranslation.value,
    startMs: selected.value.start_ms,
    endMs: selected.value.end_ms,
  });
  const idx = segments.value.findIndex((segment) => segment.id === updated.id);
  if (idx >= 0) segments.value[idx] = updated;
  selected.value = updated;
}

async function togglePlayback() {
  if (!media.value || !current.value) return;
  if (media.value.paused) await media.value.play();
  else media.value.pause();
}

function seekTo(seconds: number) {
  if (!media.value) return;
  const max = Number.isFinite(media.value.duration) ? media.value.duration : duration.value;
  media.value.currentTime = clamp(seconds, 0, Math.max(0, max || 0));
  currentTime.value = media.value.currentTime;
}

function seekBy(seconds: number) {
  seekTo(currentTime.value + seconds);
}

function stepSegment(direction: -1 | 1) {
  if (!segments.value.length) return;
  const ms = currentTime.value * 1000;
  let index = segments.value.findIndex(
    (segment) => ms >= segment.start_ms && ms < segment.end_ms,
  );
  if (index < 0) {
    index = segments.value.findIndex((segment) => segment.start_ms > ms);
    if (index < 0) index = segments.value.length - 1;
  }
  const next = clamp(index + direction, 0, segments.value.length - 1);
  chooseSegment(segments.value[next]);
}

function toggleSentenceLoop() {
  sentenceLoop.value = !sentenceLoop.value;
  if (sentenceLoop.value) {
    loopA.value = null;
    loopB.value = null;
  }
}

function setLoopPoint(point: "a" | "b") {
  if (point === "a") {
    loopA.value = currentTime.value;
    if (loopB.value !== null && loopB.value <= loopA.value) loopB.value = null;
  } else {
    loopB.value = currentTime.value;
    if (loopA.value !== null && loopB.value <= loopA.value) loopA.value = null;
  }
  sentenceLoop.value = false;
}

function clearLoop() {
  loopA.value = null;
  loopB.value = null;
  sentenceLoop.value = false;
}

async function persistPlayback(force = false) {
  if (!current.value || !media.value) return;
  const now = Date.now();
  if (!force && now - lastPersistAt < 1800) return;
  lastPersistAt = now;
  const positionMs = Math.round(media.value.currentTime * 1000);
  current.value.playback_position = positionMs;
  current.value.speed = playbackRate.value;
  current.value.volume = volume.value;
  await invoke("save_playback_state", {
    id: current.value.id,
    positionMs,
    speed: playbackRate.value,
    volume: volume.value,
  }).catch(() => {});
}

function applyPlaybackSettings() {
  if (media.value) {
    media.value.playbackRate = playbackRate.value;
    media.value.volume = volume.value;
  }
  void persistPlayback(true);
}

function onTimeUpdate() {
  if (!media.value) return;
  currentTime.value = media.value.currentTime;

  if (
    loopA.value !== null &&
    loopB.value !== null &&
    currentTime.value >= loopB.value
  ) {
    media.value.currentTime = loopA.value;
    return;
  }

  const active = activeSegment.value;
  if (
    sentenceLoop.value &&
    active &&
    currentTime.value * 1000 >= active.end_ms - 25
  ) {
    media.value.currentTime = active.start_ms / 1000;
    return;
  }

  void persistPlayback();
}

function onLoaded() {
  if (!media.value || !current.value) return;
  duration.value = Number.isFinite(media.value.duration) ? media.value.duration : 0;
  media.value.playbackRate = playbackRate.value;
  media.value.volume = volume.value;

  if (pendingResumeMs.value > 0 && duration.value > 0) {
    media.value.currentTime = Math.min(
      pendingResumeMs.value / 1000,
      Math.max(0, duration.value - 0.25),
    );
    currentTime.value = media.value.currentTime;
  }
  pendingResumeMs.value = 0;

  void invoke("update_media_duration", {
    id: current.value.id,
    durationMs: Math.round(duration.value * 1000),
  });
}

function onEnded() {
  playing.value = false;
  currentTime.value = 0;
  if (media.value) media.value.currentTime = 0;
  void persistPlayback(true);
}

function onScrub(event: Event) {
  const input = event.target as HTMLInputElement;
  seekTo(Number(input.value));
}

function onRate(event: Event) {
  playbackRate.value = Number((event.target as HTMLSelectElement).value);
  applyPlaybackSettings();
}

function onVolume(event: Event) {
  volume.value = Number((event.target as HTMLInputElement).value);
  applyPlaybackSettings();
}

function isEditableTarget(target: EventTarget | null) {
  const el = target as HTMLElement | null;
  if (!el) return false;
  return (
    el.isContentEditable ||
    ["INPUT", "TEXTAREA", "SELECT"].includes(el.tagName)
  );
}

function onKeydown(event: KeyboardEvent) {
  const mod = event.metaKey || event.ctrlKey;

  if (mod && event.key.toLowerCase() === "b") {
    event.preventDefault();
    if (event.shiftKey) toggleInspector();
    else toggleSidebar();
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
}

function fmt(value: number) {
  const total = Math.max(0, Math.floor(value || 0));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  if (hours) {
    return `${hours}:${String(minutes).padStart(2, "0")}:${String(seconds).padStart(2, "0")}`;
  }
  return `${minutes}:${String(seconds).padStart(2, "0")}`;
}

function onSystemThemeChange() {
  if (theme.value === "system") applyTheme();
}

watch(resolvedTheme, applyTheme);

onMounted(async () => {
  applyTheme();
  prefersDark.addEventListener("change", onSystemThemeChange);
  window.addEventListener("keydown", onKeydown);

  await refreshLibrary();
  const restored = items.value.find((item) => item.id === initial.activeMediaId);
  if (restored) await choose(restored, false);
  else if (openTabs.value[0]) await choose(openTabs.value[0], false);
});

onBeforeUnmount(() => {
  void persistPlayback(true);
  persistUiState();
  prefersDark.removeEventListener("change", onSystemThemeChange);
  window.removeEventListener("keydown", onKeydown);
});
</script>

<template>
  <div
    class="workbench"
    :class="{
      'sidebar-hidden': !sidebarVisible,
      'inspector-hidden': !inspectorVisible,
    }"
    :style="workbenchStyle"
  >
    <aside class="rail">
      <button class="brand" aria-label="Tsubame">T</button>
      <nav>
        <button
          v-for="item in RAILS"
          :key="item"
          :class="{ active: activeRail === item }"
          :title="item"
          @click="setRail(item)"
        >
          <span>{{
            ({
              home: "⌂",
              library: "▣",
              discover: "⌕",
              downloads: "↓",
              studio: "✦",
              jobs: "≡",
            } as Record<string, string>)[item]
          }}</span>
        </button>
      </nav>
      <button class="rail-bottom" :title="`Theme: ${theme}`" @click="cycleTheme">
        {{ themeIcon }}
      </button>
    </aside>

    <aside class="sidebar">
      <div class="resize-handle resize-handle-right" @pointerdown="beginResize('sidebar', $event)" />
      <div class="sidebar-head">
        <div>
          <p class="eyebrow">{{ activeRail.toUpperCase() }}</p>
          <h1>Tsubame</h1>
        </div>
        <button class="icon-button" @click="importFiles" title="Import media">＋</button>
      </div>

      <template v-if="activeRail === 'library' || activeRail === 'studio'">
        <div class="source-group">
          <p class="section-label">MEDIA</p>
          <button
            v-for="item in items"
            :key="item.id"
            class="media-row"
            :class="{ selected: current?.id === item.id }"
            @click="choose(item)"
          >
            <span class="media-glyph">{{ item.media_type === "video" ? "▤" : "♪" }}</span>
            <span class="media-copy">
              <strong>{{ item.title }}</strong>
              <small>{{ item.duration_ms ? fmt(item.duration_ms / 1000) : "Local media" }}</small>
            </span>
          </button>
          <button v-if="!items.length" class="empty-import" @click="importFiles">
            Import your first audio or video
          </button>
        </div>
        <div class="source-group">
          <p class="section-label">SOURCES</p>
          <div class="source-row"><span>◉</span><span>Japanese ASMR</span></div>
          <div class="source-row muted"><span>○</span><span>ASMR.one</span></div>
          <div class="source-row muted"><span>⌁</span><span>Local folders</span></div>
        </div>
      </template>

      <template v-else>
        <div class="source-group shell-placeholder">
          <p class="section-label">SECTION</p>
          <strong>{{ activeRail }}</strong>
          <small>This surface is reserved; playback remains global.</small>
        </div>
      </template>
    </aside>

    <main class="main">
      <header class="tabs">
        <button class="chrome-button" title="Toggle sidebar · Ctrl/Cmd+B" @click="toggleSidebar">
          ☰
        </button>
        <div class="tab-scroll">
          <button
            v-for="tab in openTabs"
            :key="tab.id"
            class="tab"
            :class="{ active: current?.id === tab.id }"
            @click="choose(tab, false)"
          >
            <span>{{ tab.title }}</span>
            <span class="tab-close" title="Close tab" @click.stop="closeTab(tab.id)">×</span>
          </button>
          <div v-if="!openTabs.length" class="tab active">Library</div>
        </div>
        <button class="tab-add" title="Import media" @click="importFiles">＋</button>
        <button
          class="chrome-button"
          title="Toggle inspector · Ctrl/Cmd+Shift+B"
          @click="toggleInspector"
        >
          ◫
        </button>
      </header>

      <section v-show="mediaWorkspaceVisible" class="content media-workspace">
        <div class="hero" :class="{ 'video-hero': current?.media_type === 'video' }">
          <div v-if="!current || current.media_type === 'audio'" class="artwork">
            <span>{{ current ? "♪" : "T" }}</span>
          </div>

          <component
            :is="current?.media_type === 'video' ? 'video' : 'audio'"
            v-if="current"
            ref="media"
            :src="sourceUrl"
            :class="current.media_type === 'video' ? 'video-stage' : 'audio-engine'"
            playsinline
            @timeupdate="onTimeUpdate"
            @loadedmetadata="onLoaded"
            @play="playing = true"
            @pause="playing = false; persistPlayback(true)"
            @ended="onEnded"
          />

          <div class="hero-copy">
            <p class="eyebrow">{{ current ? "NOW WORKING" : "LOCAL-FIRST ASMR WORKBENCH" }}</p>
            <h2>{{ current?.title || "Your library, transcript and dub in one place." }}</h2>
            <p>
              {{
                current
                  ? "Original media · sentence timeline · modular processing"
                  : "Import media to start. Provider-backed ASR and dubbing land in later PRs."
              }}
            </p>
            <div class="hero-actions">
              <button v-if="!current" class="primary" @click="importFiles">Import Media</button>
              <button v-else class="primary" @click="togglePlayback">
                {{ playing ? "Pause" : "Play" }}
              </button>
              <button v-if="current" class="secondary" @click="setRail('studio')">Open Studio</button>
            </div>
          </div>
        </div>

        <div v-if="current" class="timeline-head">
          <div>
            <p class="section-label">TIMELINE</p>
            <strong>{{ segments.length }} segments</strong>
          </div>
          <div class="timeline-actions">
            <button :class="{ active: sentenceLoop }" @click="toggleSentenceLoop">
              Sentence Loop
            </button>
            <button :class="{ active: loopA !== null }" @click="setLoopPoint('a')">
              A {{ loopA === null ? "" : fmt(loopA) }}
            </button>
            <button :class="{ active: loopB !== null }" @click="setLoopPoint('b')">
              B {{ loopB === null ? "" : fmt(loopB) }}
            </button>
            <button v-if="loopA !== null || loopB !== null || sentenceLoop" @click="clearLoop">
              Clear
            </button>
          </div>
        </div>

        <div v-if="current" class="segments">
          <button
            v-for="segment in segments"
            :key="segment.id"
            class="segment"
            :class="{
              active: selected?.id === segment.id,
              playing: activeSegment?.id === segment.id,
            }"
            @click="chooseSegment(segment)"
          >
            <span class="time">{{ fmt(segment.start_ms / 1000) }}</span>
            <span class="segment-copy">
              <strong>{{ segment.source_text || "Untitled segment" }}</strong>
              <small>{{ segment.translated_text || "No translation yet" }}</small>
            </span>
          </button>
          <div v-if="!segments.length" class="empty-state">
            <strong>No sentence timeline yet</strong>
            <span>ASR providers and subtitle import will populate canonical Segments.</span>
          </div>
        </div>
      </section>

      <section v-show="!mediaWorkspaceVisible" class="content section-placeholder">
        <p class="eyebrow">{{ activeRail.toUpperCase() }}</p>
        <h2>{{ activeRail === "home" ? "Continue where you left off." : `${activeRail} is ready for its next adapter.` }}</h2>
        <p>
          The global player stays alive while product surfaces change. This is the workbench contract,
          not a page-based player.
        </p>
        <button v-if="current" class="secondary" @click="setRail('library')">
          Return to {{ current.title }}
        </button>
      </section>
    </main>

    <aside class="inspector">
      <div class="resize-handle resize-handle-left" @pointerdown="beginResize('inspector', $event)" />
      <template v-if="selected">
        <p class="eyebrow">SEGMENT {{ selected.ordinal + 1 }}</p>
        <h3>Sentence Inspector</h3>
        <label>
          Original
          <textarea v-model="draftSource" />
        </label>
        <label>
          Translation
          <textarea v-model="draftTranslation" />
        </label>
        <div class="time-grid">
          <label>Start<input :value="fmt(selected.start_ms / 1000)" readonly /></label>
          <label>End<input :value="fmt(selected.end_ms / 1000)" readonly /></label>
        </div>
        <button class="primary wide" @click="saveSegment">Save changes</button>
        <button class="secondary wide" disabled>Regenerate Segment · PR #9</button>
      </template>
      <template v-else>
        <p class="eyebrow">INSPECTOR</p>
        <h3>{{ current ? "Track" : "Nothing selected" }}</h3>
        <p class="inspector-help">
          {{
            current
              ? "Select a sentence to edit transcript, translation and later voice settings."
              : "Select media or a sentence to inspect it here."
          }}
        </p>
        <div v-if="current" class="track-facts">
          <div><span>Type</span><strong>{{ current.media_type }}</strong></div>
          <div><span>Duration</span><strong>{{ fmt(duration) }}</strong></div>
          <div><span>Speed</span><strong>{{ playbackRate.toFixed(2) }}×</strong></div>
          <div><span>Volume</span><strong>{{ Math.round(volume * 100) }}%</strong></div>
        </div>
      </template>
    </aside>

    <footer class="playerbar">
      <div class="now-playing">
        <div class="mini-art">{{ current?.media_type === "video" ? "▤" : "♪" }}</div>
        <div>
          <strong>{{ current?.title || "Nothing Playing" }}</strong>
          <small>
            {{
              sentenceLoop
                ? "Sentence loop"
                : loopA !== null || loopB !== null
                  ? "A–B loop"
                  : "Original"
            }}
          </small>
        </div>
      </div>

      <div class="transport">
        <div class="transport-buttons">
          <button title="Previous sentence · [" @click="stepSegment(-1)">‹|</button>
          <button title="Back 15 seconds" @click="seekBy(-15)">−15</button>
          <button class="play" title="Play/Pause · Space" @click="togglePlayback">
            {{ playing ? "Ⅱ" : "▶" }}
          </button>
          <button title="Forward 15 seconds" @click="seekBy(15)">+15</button>
          <button title="Next sentence · ]" @click="stepSegment(1)">|›</button>
        </div>
        <div class="scrubber">
          <span>{{ fmt(currentTime) }}</span>
          <input
            type="range"
            min="0"
            :max="duration || 1"
            step="0.1"
            :value="currentTime"
            @input="onScrub"
          />
          <span>{{ fmt(duration) }}</span>
        </div>
      </div>

      <div class="player-meta">
        <select :value="playbackRate" title="Playback speed" @change="onRate">
          <option v-for="rate in [0.75, 1, 1.25, 1.5, 2]" :key="rate" :value="rate">
            {{ rate }}×
          </option>
        </select>
        <label class="volume-control" title="Volume">
          <span>◖</span>
          <input
            type="range"
            min="0"
            max="1"
            step="0.02"
            :value="volume"
            @input="onVolume"
          />
        </label>
      </div>
    </footer>
  </div>
</template>
