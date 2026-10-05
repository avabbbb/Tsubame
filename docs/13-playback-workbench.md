# Workbench Playback Contract

This document describes the user-visible behavior implemented by PR #3.

## Persistent shell

The Tsubame workbench keeps these preferences across restart:

- active rail section;
- Context Sidebar visibility and width;
- Inspector visibility and width;
- open media tabs;
- active media tab;
- theme preference.

Layout persistence is UI state. Canonical media/segment state remains in SQLite.

## Playback persistence

Per media item, SQLite persists:

- playback position;
- playback speed;
- volume.

Opening media resumes from the previous position without prompting, matching the expected behavior for long-form media players.

## Global player

Playback belongs to the App Shell, not to a route/page.

Switching between Home, Library, Discover, Downloads, Studio and Jobs must not intentionally stop playback.

## Segment navigation

When Segments exist:

- click a Segment to seek to its start;
- `[` moves to the previous Segment;
- `]` moves to the next Segment;
- current playback highlights the active Segment;
- Sentence Loop repeats the active Segment.

## A–B loop

Users may place loop points at the current playhead:

- `Shift+A` sets point A;
- `Shift+B` sets point B;
- `Escape` clears active loop state.

Sentence Loop and A–B loop are mutually exclusive.

## Playback shortcuts

- `Space` — play/pause;
- `Left / Right` — seek 5 seconds;
- `Shift+Left / Shift+Right` — seek 15 seconds;
- `[` / `]` — previous/next Segment;
- `L` — toggle Sentence Loop;
- `Ctrl/Cmd+B` — toggle Context Sidebar;
- `Ctrl/Cmd+Shift+B` — toggle Inspector.

Keyboard playback shortcuts must not fire while typing in an input, textarea, select or content-editable surface.

## Video

Video uses the same global playback state and transport controls as audio. In Library/Studio it can render inline in the main media header. Navigating away may visually hide the stage while preserving the media element and playback state.

## Visual direction

Tsubame keeps the Codex/VS Code workbench geometry while using Apple Music as the media-product reference:

- artwork/media-led hierarchy;
- polished transport controls;
- softly layered navigation surfaces;
- content remains readable and denser than a consumer-only music app;
- light and dark modes are first-class;
- translucent/material effects belong mainly to navigation/player chrome, not every editor surface.
