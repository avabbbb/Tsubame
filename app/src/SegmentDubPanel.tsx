import { useEffect, useRef, useState } from "react";
import type { KeyboardEvent as ReactKeyboardEvent } from "react";
import { invoke } from "@tauri-apps/api/core";
import { canCancel, jobProgress, useJobs } from "./jobs";
import type { ResolvedTtsAsset, Segment, SpatialRenderOutcome } from "./types";

type Props = {
  segment: Segment;
  /** True while the Inspector holds unsaved edits for this sentence. */
  unsaved: boolean;
};

/**
 * Inspector section for the active clip: its format, with the explicit
 * re-spatialize (and its Cancel) tucked into a "More" (⋯) menu so it does not
 * compete with the Synthesize/Regenerate action. Synthesis lives in that
 * single action and listening in the takes list / context preview
 * (SegmentRegeneratePanel).
 */
export default function SegmentDubPanel({ segment, unsaved }: Props) {
  const [asset, setAsset] = useState<ResolvedTtsAsset | null>(null);
  const [note, setNote] = useState<string | null>(null);
  const [spatialBusy, setSpatialBusy] = useState(false);
  const [render, setRender] = useState<SpatialRenderOutcome | null>(null);
  const { jobs: spatialJobs, cancel } = useJobs({ segment_id: segment.id, kind: "spatial" });
  const spatialJob = spatialJobs.find(canCancel) ?? null;
  const spatialFraction = spatialJob ? jobProgress(spatialJob) : null;
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement | null>(null);
  const triggerRef = useRef<HTMLButtonElement | null>(null);
  const menuId = `spatial-menu-${segment.id}`;

  useEffect(() => {
    setNote(null);
    void invoke<ResolvedTtsAsset | null>("get_tts_asset", { segmentId: segment.id })
      .then(setAsset)
      .catch((error) => {
        setAsset(null);
        setNote(String(error));
      });
  }, [segment.id, segment.revision, segment.tts_asset_key]);

  // Another sentence starts with the menu closed.
  useEffect(() => setMenuOpen(false), [segment.id]);

  // Minimal disclosure menu: focus the first item on open; a click outside closes.
  useEffect(() => {
    if (!menuOpen) return;
    menuRef.current?.querySelector<HTMLElement>("[role=menuitem]")?.focus();
    const onPointer = (event: PointerEvent) => {
      const target = event.target as Node;
      if (menuRef.current?.contains(target) || triggerRef.current?.contains(target)) return;
      setMenuOpen(false);
    };
    window.addEventListener("pointerdown", onPointer);
    return () => window.removeEventListener("pointerdown", onPointer);
  }, [menuOpen]);

  function closeMenu(refocus: boolean) {
    setMenuOpen(false);
    if (refocus) triggerRef.current?.focus();
  }

  function onMenuKey(event: ReactKeyboardEvent<HTMLDivElement>) {
    const items = [...(menuRef.current?.querySelectorAll<HTMLElement>("[role=menuitem]") ?? [])];
    const index = items.indexOf(document.activeElement as HTMLElement);
    if (event.key === "Escape") {
      event.preventDefault();
      closeMenu(true);
    } else if (event.key === "Tab") {
      setMenuOpen(false);
    } else if (event.key === "ArrowDown" || event.key === "ArrowUp") {
      event.preventDefault();
      const step = event.key === "ArrowDown" ? 1 : -1;
      items[(index + step + items.length) % items.length]?.focus();
    } else if (event.key === "Home" || event.key === "End") {
      event.preventDefault();
      items[event.key === "Home" ? 0 : items.length - 1]?.focus();
    }
  }

  // A render belongs to one TTS clip; forget it when the clip changes.
  useEffect(() => {
    setRender((current) => (current && current.tts_asset_key === asset?.key ? current : null));
  }, [segment.id, asset?.key]);

  async function spatialize() {
    setSpatialBusy(true);
    setNote(null);
    try {
      const outcome = await invoke<SpatialRenderOutcome>("spatialize_segment", {
        segmentId: segment.id,
      });
      setRender(outcome);
      setNote(
        outcome.cache_hit
          ? "Reused a cached spatial render. Play in context to hear it."
          : "Spatialized. Play in context to hear it.",
      );
    } catch (error) {
      setNote(String(error));
    } finally {
      setSpatialBusy(false);
    }
  }

  async function stopSpatial() {
    if (!spatialJob) return;
    try {
      await cancel(spatialJob.id);
    } catch (error) {
      setNote(String(error));
    }
  }

  if (!asset && !note) return null;

  const spatialBlocker = unsaved
    ? "Save changes first"
    : segment.dirty_tts
      ? "Regenerate the current translation first"
      : null;
  const spatialDisabled = spatialBusy || spatialBlocker !== null;
  const spatialLabel = render
    ? "Re-spatialize again (no new take)"
    : "Re-spatialize only (no new take)";

  return (
    <div className="segment-state">
      <div className="section-head">
        <p className="section-label">SPATIAL AUDIO</p>
        {asset && (
          <div className="more-menu">
            <button
              ref={triggerRef}
              className="more-trigger"
              aria-label="More spatial audio actions"
              title="More"
              aria-haspopup="menu"
              aria-expanded={menuOpen}
              aria-controls={menuOpen ? menuId : undefined}
              onClick={() => setMenuOpen((open) => !open)}
            >
              ⋯
            </button>
            {menuOpen && (
              <div
                ref={menuRef}
                id={menuId}
                role="menu"
                aria-label="Spatial audio actions"
                className="more-menu-list"
                onKeyDown={onMenuKey}
              >
                <button
                  role="menuitem"
                  aria-disabled={spatialDisabled}
                  title={spatialBlocker ?? "Follow the original sentence's position in the stereo field"}
                  onClick={() => {
                    if (spatialDisabled) return;
                    closeMenu(true);
                    void spatialize();
                  }}
                >
                  <span>{spatialLabel}</span>
                  <small>
                    {spatialBusy
                      ? "Running…"
                      : (spatialBlocker ?? "Re-render the active clip's spatial audio")}
                  </small>
                </button>
                {spatialJob && (
                  <button
                    role="menuitem"
                    onClick={() => {
                      closeMenu(true);
                      void stopSpatial();
                    }}
                  >
                    <span>Cancel re-spatialize</span>
                  </button>
                )}
              </div>
            )}
          </div>
        )}
      </div>
      {asset && (
        <p className="inspector-help">
          Active clip: {(asset.duration_ms / 1000).toFixed(2)} s · {asset.sample_rate / 1000} kHz ·{" "}
          {asset.channels === 1 ? "mono" : `${asset.channels} ch`}
          {segment.dirty_tts ? " · out of date" : ""}
        </p>
      )}
      {spatialBusy && (
        <p className="inspector-help" role="status">
          {spatialFraction !== null
            ? `Re-spatializing ${Math.round(spatialFraction * 100)}%… (Cancel in ⋯)`
            : "Re-spatializing… (Cancel in ⋯)"}
        </p>
      )}
      {note && <p className="inspector-help">{note}</p>}
    </div>
  );
}
