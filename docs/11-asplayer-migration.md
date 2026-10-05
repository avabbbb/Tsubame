# ASPlayer baseline migration

PR1 starts from ASPlayer's current `main` commit:

```
6b11fca34da3db211c7bff153b46f999d6cd1667
```

Upstream: https://github.com/yumili426/ASPlayer

## What is carried forward first

- Tauri 2 + Vue/TypeScript desktop shape;
- local media import/playback using Tauri's asset protocol;
- SQLite-backed media state;
- sentence/segment timeline concept;
- click-to-seek interaction;
- revision-protected sentence editing.

## What is intentionally not copied as-is

- hard-coded Whisper-as-the-transcription-architecture;
- page-level ASPlayer information architecture;
- model-specific settings embedded into product surfaces;
- ASPlayer product naming/paths/identifiers;
- GalGame direction.

## First migration delta

Tsubame immediately renames the canonical subtitle row to **Segment** and adds `revision` so human/agent edits can use optimistic concurrency from the beginning.

The first frontend shell also applies the already-approved **Apple Music visual language × Codex workbench layout** instead of preserving ASPlayer's existing page composition.

Subsequent commits in this PR can port/refactor additional proven playback and subtitle behavior while keeping these Tsubame contracts intact.
