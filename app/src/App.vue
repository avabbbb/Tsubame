<script setup lang="ts">
import { computed, nextTick, onMounted, ref } from "vue";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { MediaItem, Segment } from "./types";

const items = ref<MediaItem[]>([]);
const current = ref<MediaItem | null>(null);
const segments = ref<Segment[]>([]);
const selected = ref<Segment | null>(null);
const audio = ref<HTMLMediaElement | null>(null);
const currentTime = ref(0);
const duration = ref(0);
const playing = ref(false);
const draftSource = ref("");
const draftTranslation = ref("");
const activeRail = ref("library");

const sourceUrl = computed(() => current.value ? convertFileSrc(current.value.path) : "");

async function refreshLibrary() {
  items.value = await invoke<MediaItem[]>("list_media");
}

async function importFiles() {
  const picked = await open({
    multiple: true,
    filters: [{
      name: "Media",
      extensions: ["mp3","m4a","wav","flac","ogg","opus","aac","m4b","mp4","m4v","webm","mkv","mov","avi"]
    }]
  });
  if (!picked) return;
  const paths = Array.isArray(picked) ? picked : [picked];
  for (const path of paths) await invoke("import_media", { path });
  await refreshLibrary();
  if (!current.value && items.value[0]) await choose(items.value[0]);
}

async function choose(item: MediaItem) {
  current.value = item;
  selected.value = null;
  segments.value = await invoke<Segment[]>("get_segments", { mediaId: item.id });
  await nextTick();
  if (audio.value) {
    audio.value.currentTime = item.playback_position / 1000;
    audio.value.load();
  }
}

function chooseSegment(segment: Segment) {
  selected.value = segment;
  draftSource.value = segment.source_text;
  draftTranslation.value = segment.translated_text;
  if (audio.value) audio.value.currentTime = segment.start_ms / 1000;
}

async function saveSegment() {
  if (!selected.value) return;
  const updated = await invoke<Segment>("update_segment", {
    id: selected.value.id,
    expectedRevision: selected.value.revision,
    sourceText: draftSource.value,
    translatedText: draftTranslation.value,
    startMs: selected.value.start_ms,
    endMs: selected.value.end_ms
  });
  const idx = segments.value.findIndex(s => s.id === updated.id);
  if (idx >= 0) segments.value[idx] = updated;
  selected.value = updated;
}

async function togglePlayback() {
  if (!audio.value) return;
  if (audio.value.paused) await audio.value.play();
  else audio.value.pause();
}

function onTimeUpdate() {
  if (!audio.value) return;
  currentTime.value = audio.value.currentTime;
}

function onLoaded() {
  if (!audio.value || !current.value) return;
  duration.value = Number.isFinite(audio.value.duration) ? audio.value.duration : 0;
  invoke("update_media_duration", { id: current.value.id, durationMs: Math.round(duration.value * 1000) }).catch(() => {});
}

function fmt(value: number) {
  const total = Math.max(0, Math.floor(value));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${String(s).padStart(2, "0")}`;
}

onMounted(refreshLibrary);
</script>

<template>
  <div class="workbench">
    <aside class="rail">
      <button class="brand" aria-label="Tsubame">Y</button>
      <nav>
        <button v-for="item in ['home','library','discover','downloads','studio','jobs']"
          :key="item" :class="{active: activeRail === item}" @click="activeRail = item"
          :title="item">
          <span>{{ ({home:'⌂',library:'▣',discover:'⌕',downloads:'↓',studio:'✦',jobs:'≡'} as Record<string,string>)[item] }}</span>
        </button>
      </nav>
      <button class="rail-bottom" title="Settings">⚙</button>
    </aside>

    <aside class="sidebar">
      <div class="sidebar-head">
        <div>
          <p class="eyebrow">LIBRARY</p>
          <h1>Tsubame</h1>
        </div>
        <button class="icon-button" @click="importFiles" title="Import media">＋</button>
      </div>
      <div class="source-group">
        <p class="section-label">MEDIA</p>
        <button v-for="item in items" :key="item.id" class="media-row"
          :class="{selected: current?.id === item.id}" @click="choose(item)">
          <span class="media-glyph">{{ item.media_type === 'video' ? '▤' : '♪' }}</span>
          <span class="media-copy">
            <strong>{{ item.title }}</strong>
            <small>{{ item.duration_ms ? fmt(item.duration_ms / 1000) : 'Local media' }}</small>
          </span>
        </button>
        <button v-if="!items.length" class="empty-import" @click="importFiles">Import your first audio or video</button>
      </div>
      <div class="source-group">
        <p class="section-label">SOURCES</p>
        <div class="source-row"><span>◉</span><span>Japanese ASMR</span></div>
        <div class="source-row muted"><span>○</span><span>ASMR.one</span></div>
        <div class="source-row muted"><span>⌁</span><span>Local folders</span></div>
      </div>
    </aside>

    <main class="main">
      <header class="tabs">
        <div class="tab active">{{ current?.title || 'Library' }}</div>
        <button class="tab-add">＋</button>
      </header>

      <section class="content">
        <div class="hero">
          <div class="artwork">
            <span>{{ current ? '♪' : 'Y' }}</span>
          </div>
          <div class="hero-copy">
            <p class="eyebrow">{{ current ? 'NOW WORKING' : 'LOCAL-FIRST ASMR WORKBENCH' }}</p>
            <h2>{{ current?.title || 'Your library, transcript and dub in one place.' }}</h2>
            <p>{{ current ? 'Original media · sentence timeline · modular processing' : 'Import media to start. Provider-backed ASR and dubbing land in later PRs.' }}</p>
            <button v-if="!current" class="primary" @click="importFiles">Import Media</button>
          </div>
        </div>

        <div v-if="current" class="timeline-head">
          <div>
            <p class="section-label">TIMELINE</p>
            <strong>{{ segments.length }} segments</strong>
          </div>
          <div class="timeline-actions">
            <button>Transcript</button>
            <button>Translate</button>
            <button>Voice</button>
          </div>
        </div>

        <div v-if="current" class="segments">
          <button v-for="segment in segments" :key="segment.id" class="segment"
            :class="{active: selected?.id === segment.id}" @click="chooseSegment(segment)">
            <span class="time">{{ fmt(segment.start_ms / 1000) }}</span>
            <span class="segment-copy">
              <strong>{{ segment.source_text || 'Untitled segment' }}</strong>
              <small>{{ segment.translated_text || 'No translation yet' }}</small>
            </span>
          </button>
          <div v-if="!segments.length" class="empty-state">
            <strong>No sentence timeline yet</strong>
            <span>ASR providers and subtitle import will populate canonical Segments.</span>
          </div>
        </div>
      </section>
    </main>

    <aside class="inspector">
      <template v-if="selected">
        <p class="eyebrow">SEGMENT {{ selected.ordinal + 1 }}</p>
        <h3>Sentence Inspector</h3>
        <label>Original<textarea v-model="draftSource" /></label>
        <label>Translation<textarea v-model="draftTranslation" /></label>
        <div class="time-grid">
          <label>Start<input :value="fmt(selected.start_ms / 1000)" readonly /></label>
          <label>End<input :value="fmt(selected.end_ms / 1000)" readonly /></label>
        </div>
        <button class="primary wide" @click="saveSegment">Save changes</button>
        <button class="secondary wide" disabled>Regenerate Segment · coming next</button>
      </template>
      <template v-else>
        <p class="eyebrow">INSPECTOR</p>
        <h3>{{ current ? 'Track' : 'Nothing selected' }}</h3>
        <p class="inspector-help">{{ current ? 'Select a sentence to edit timing, transcript, translation and voice settings.' : 'Select media or a sentence to inspect it here.' }}</p>
      </template>
    </aside>

    <footer class="playerbar">
      <div class="now-playing">
        <div class="mini-art">♪</div>
        <div><strong>{{ current?.title || 'Nothing Playing' }}</strong><small>Original</small></div>
      </div>
      <div class="transport">
        <div class="transport-buttons">
          <button>−15</button>
          <button class="play" @click="togglePlayback">{{ playing ? 'Ⅱ' : '▶' }}</button>
          <button>+15</button>
        </div>
        <div class="scrubber">
          <span>{{ fmt(currentTime) }}</span>
          <input type="range" min="0" :max="duration || 1" step="0.1" :value="currentTime"
            @input="audio && (audio.currentTime = Number(($event.target as HTMLInputElement).value))" />
          <span>{{ fmt(duration) }}</span>
        </div>
      </div>
      <div class="player-meta"><span>Original⌄</span><span>1.0×</span><span>Processing 0</span></div>
      <component :is="current?.media_type === 'video' ? 'video' : 'audio'" v-if="current"
        ref="audio" :src="sourceUrl" class="hidden-media"
        @timeupdate="onTimeUpdate" @loadedmetadata="onLoaded"
        @play="playing = true" @pause="playing = false" />
    </footer>
  </div>
</template>
