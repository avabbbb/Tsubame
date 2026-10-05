import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import {
  formatEditorTime,
  parseEditorTime,
  segmentDraft,
} from "./segmentEditor";
import type { Segment, SegmentDraft } from "./types";

type Props = {
  segment: Segment | null;
  onUpdated: (segment: Segment) => void;
};

function sameDraft(left: SegmentDraft, right: SegmentDraft) {
  return (
    left.source_text === right.source_text &&
    left.translated_text === right.translated_text &&
    left.start_ms === right.start_ms &&
    left.end_ms === right.end_ms
  );
}

export default function SegmentInspector({ segment, onUpdated }: Props) {
  const [source, setSource] = useState("");
  const [translation, setTranslation] = useState("");
  const [start, setStart] = useState("00:00:00.000");
  const [end, setEnd] = useState("00:00:00.000");
  const [undoStack, setUndoStack] = useState<SegmentDraft[]>([]);
  const [redoStack, setRedoStack] = useState<SegmentDraft[]>([]);
  const [conflict, setConflict] = useState<Segment | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!segment) return;
    setSource(segment.source_text);
    setTranslation(segment.translated_text);
    setStart(formatEditorTime(segment.start_ms));
    setEnd(formatEditorTime(segment.end_ms));
    setConflict(null);
    setError(null);
  }, [segment?.id, segment?.revision]);

  useEffect(() => {
    setUndoStack([]);
    setRedoStack([]);
  }, [segment?.id]);

  const draft = useMemo<SegmentDraft | null>(() => {
    const startMs = parseEditorTime(start);
    const endMs = parseEditorTime(end);
    if (startMs === null || endMs === null) return null;
    return {
      source_text: source,
      translated_text: translation,
      start_ms: startMs,
      end_ms: endMs,
    };
  }, [end, source, start, translation]);

  const dirty = useMemo(() => {
    if (!segment || !draft) return false;
    return !sameDraft(segmentDraft(segment), draft);
  }, [draft, segment]);

  async function mutate(
    base: Segment,
    next: SegmentDraft,
    recordUndo: boolean,
  ) {
    if (next.start_ms < 0 || next.end_ms <= next.start_ms) {
      throw new Error("End time must be after start time.");
    }

    const updated = await invoke<Segment>("update_segment", {
      id: base.id,
      expectedRevision: base.revision,
      sourceText: next.source_text,
      translatedText: next.translated_text,
      startMs: next.start_ms,
      endMs: next.end_ms,
    });

    if (recordUndo) {
      setUndoStack((items) => [...items, segmentDraft(base)]);
      setRedoStack([]);
    }
    onUpdated(updated);
    return updated;
  }

  async function handleConflict(errorValue: unknown) {
    const message = String(errorValue);
    if (!segment || !message.includes("revision conflict")) {
      setError(message);
      return;
    }

    const latest = await invoke<Segment | null>("get_segment", {
      id: segment.id,
    });
    if (latest) {
      setConflict(latest);
      setError("This sentence changed after you opened it.");
    } else {
      setError("This sentence no longer exists.");
    }
  }

  async function save() {
    if (!segment) return;
    if (!draft) {
      setError("Use a time like 00:01:23.456.");
      return;
    }
    if (!dirty) return;

    setSaving(true);
    setError(null);
    try {
      await mutate(segment, draft, true);
    } catch (errorValue) {
      await handleConflict(errorValue);
    } finally {
      setSaving(false);
    }
  }

  async function undo() {
    if (!segment || undoStack.length === 0) return;
    const target = undoStack.at(-1);
    if (!target) return;
    const current = segmentDraft(segment);

    setSaving(true);
    setError(null);
    try {
      const updated = await mutate(segment, target, false);
      setUndoStack((items) => items.slice(0, -1));
      setRedoStack((items) => [...items, current]);
      onUpdated(updated);
    } catch (errorValue) {
      await handleConflict(errorValue);
    } finally {
      setSaving(false);
    }
  }

  async function redo() {
    if (!segment || redoStack.length === 0) return;
    const target = redoStack.at(-1);
    if (!target) return;
    const current = segmentDraft(segment);

    setSaving(true);
    setError(null);
    try {
      const updated = await mutate(segment, target, false);
      setRedoStack((items) => items.slice(0, -1));
      setUndoStack((items) => [...items, current]);
      onUpdated(updated);
    } catch (errorValue) {
      await handleConflict(errorValue);
    } finally {
      setSaving(false);
    }
  }

  function reloadLatest() {
    if (!conflict) return;
    onUpdated(conflict);
    setConflict(null);
    setError(null);
  }

  async function overwriteMine() {
    if (!conflict || !draft) return;
    setSaving(true);
    setError(null);
    try {
      await mutate(conflict, draft, true);
      setConflict(null);
    } catch (errorValue) {
      await handleConflict(errorValue);
    } finally {
      setSaving(false);
    }
  }

  if (!segment) {
    return (
      <>
        <p className="eyebrow">INSPECTOR</p>
        <h3>Nothing selected</h3>
        <p className="inspector-help">
          Select a sentence to edit transcript, translation, timing and provenance.
        </p>
      </>
    );
  }

  return (
    <>
      <div className="inspector-title-row">
        <div>
          <p className="eyebrow">SEGMENT {segment.ordinal + 1}</p>
          <h3>Sentence Inspector</h3>
        </div>
        <span className="revision-badge">r{segment.revision}</span>
      </div>

      <div className="history-actions">
        <button disabled={saving || undoStack.length === 0} onClick={() => void undo()}>
          ↶ Undo
        </button>
        <button disabled={saving || redoStack.length === 0} onClick={() => void redo()}>
          ↷ Redo
        </button>
      </div>

      {error && (
        <div className={`editor-alert ${conflict ? "conflict" : ""}`}>
          <strong>{conflict ? "Revision conflict" : "Cannot save"}</strong>
          <span>{error}</span>
          {conflict && (
            <div className="alert-actions">
              <button onClick={reloadLatest}>Reload latest</button>
              <button className="danger-soft" onClick={() => void overwriteMine()}>
                Keep mine
              </button>
            </div>
          )}
        </div>
      )}

      <label>
        Original
        <textarea value={source} onChange={(event) => setSource(event.target.value)} />
      </label>
      <label>
        Translation
        <textarea
          value={translation}
          onChange={(event) => setTranslation(event.target.value)}
        />
      </label>

      <div className="time-grid">
        <label>
          Start
          <input value={start} onChange={(event) => setStart(event.target.value)} />
        </label>
        <label>
          End
          <input value={end} onChange={(event) => setEnd(event.target.value)} />
        </label>
      </div>

      <div className="segment-state">
        <p className="section-label">DEPENDENCIES</p>
        <div className="badge-row">
          {segment.dirty_translation && <span className="state-badge">Translation dirty</span>}
          {segment.dirty_tts && <span className="state-badge">TTS dirty</span>}
          {segment.dirty_mix && <span className="state-badge">Mix dirty</span>}
          {segment.dirty_subtitle && <span className="state-badge">Subtitle dirty</span>}
          {!segment.dirty_translation &&
            !segment.dirty_tts &&
            !segment.dirty_mix &&
            !segment.dirty_subtitle && <span className="state-badge clean">Clean</span>}
        </div>
      </div>

      <div className="provenance-list">
        <p className="section-label">PROVENANCE</p>
        <div><span>Transcript</span><strong>{segment.transcript_provenance || "Unknown"}</strong></div>
        <div><span>ASR</span><strong>{segment.asr_provenance || "—"}</strong></div>
        <div><span>Refine</span><strong>{segment.refine_provenance || "—"}</strong></div>
        <div><span>Translation</span><strong>{segment.translation_provenance || "—"}</strong></div>
        <div><span>TTS</span><strong>{segment.tts_provenance || "—"}</strong></div>
      </div>

      <button className="primary wide" disabled={saving || !dirty || !draft} onClick={() => void save()}>
        {saving ? "Saving…" : dirty ? "Save changes" : "Saved"}
      </button>
      <button className="secondary wide" disabled>
        Regenerate Segment · PR #10
      </button>
    </>
  );
}
