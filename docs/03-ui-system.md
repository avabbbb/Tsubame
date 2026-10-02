# UI System — Yuzuki Workbench

## Canonical shell

```text
┌────┬──────────────┬──────────────────────────────────────┬──────────────────┐
│    │              │ Tabs                                 │                  │
│Rail│ Context      ├──────────────────────────────────────┤ Inspector        │
│    │ Sidebar      │                                      │                  │
│    │              │          Main Workspace              │                  │
│    │              │                                      │                  │
├────┴──────────────┴──────────────────────────────────────┴──────────────────┤
│ Global Player / Job Status Bar                                               │
└───────────────────────────────────────────────────────────────────────────────┘
```

## Responsibilities

### Activity Rail

48–52 px. Only switches major product modules:

Home, Library, Discover, Downloads, Studio, Jobs, Settings.

It never becomes a detailed navigation tree.

### Context Sidebar

Default ~240 px, resizable ~200–360 px.

Its contents change with the selected Rail module:
- Library collections and sources;
- current Work/Track tree;
- processing stages and jobs;
- Discover filters;
- Settings sections.

### Tabs

Tabs represent **open work objects**, not app modules.

Examples:
- `RJ014... / Track 03`
- `Download Queue`
- `Search · 柚木`
- `Provider · Alibaba Model Studio`

### Main Workspace

The active work surface:
- player;
- library grid/list;
- timeline + waveform;
- sentence list/editor;
- source search;
- job details;
- provider setup.

### Inspector

Default ~300 px, resizable ~260–420 px.

The Inspector edits the currently selected object.

When a Segment is selected it shows:
- start/end;
- original text;
- translation;
- ASR/refinement provenance;
- TTS provider/model/voice reference;
- original/generated preview;
- regenerate action.

### Bottom Bar

Global and persistent.

It owns:
- playback controls;
- current work/track;
- position/duration;
- variant selector;
- speed/volume;
- compact job/download status.

Playback does not stop when navigating elsewhere.

## Layout modes

- Browse: Rail + Sidebar + Main
- Edit: Rail + Sidebar + Main + Inspector
- Focus playback: Main + Bottom Bar
- Small window: hide Inspector first, then collapse Sidebar

## Visual language

Target: **Notion × Codex × Linear**, not a card-heavy consumer dashboard.

Rules:
- compact 12–13 px UI text;
- 1 px low-contrast separators;
- limited shadow;
- ~6 px small radius;
- 8–12 px panel padding;
- 16–18 px icons;
- neutral surfaces;
- secondary actions appear on hover/focus where appropriate;
- information hierarchy through spacing/lines/typography rather than nested cards.

## Timeline

The sentence timeline is not a read-only subtitle panel.

It must support:
- click to seek;
- current sentence highlight;
- text editing;
- timing editing;
- waveform context;
- sentence loop;
- original/generated preview;
- dirty status;
- `Ctrl/Cmd + Enter` regenerate current Segment.
