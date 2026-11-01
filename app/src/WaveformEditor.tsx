import { useEffect, useRef, useState } from "react";
import WaveSurfer from "wavesurfer.js";
import RegionsPlugin from "wavesurfer.js/dist/plugins/regions.esm.js";
import TimelinePlugin from "wavesurfer.js/dist/plugins/timeline.esm.js";
import type { Segment } from "./types";

type Props = {
  media: HTMLMediaElement | null;
  sourceUrl: string;
  segments: Segment[];
  selectedId: number | null;
  onSelect: (segment: Segment) => void;
  onTimingCommit: (segment: Segment, startMs: number, endMs: number) => void;
};

type Regions = ReturnType<typeof RegionsPlugin.create>;
type Instance = { wave: WaveSurfer; regions: Regions };

function drawRegions(regions: Regions, segments: Segment[], selectedId: number | null) {
  regions.clearRegions();
  for (const segment of segments) {
    const selected = segment.id === selectedId;
    regions.addRegion({
      id: String(segment.id),
      start: segment.start_ms / 1000,
      end: segment.end_ms / 1000,
      // Yellow = dub pushed later by a regenerate (docs/20); red = selected.
      color: selected
        ? "rgba(250, 45, 72, .22)"
        : segment.dub_pushed_ms > 0
          ? "rgba(255, 214, 10, .28)"
          : "rgba(142, 142, 147, .10)",
      drag: true,
      resize: true,
      minLength: 0.05,
      content: String(segment.ordinal + 1),
    });
  }
}

export default function WaveformEditor({
  media,
  sourceUrl,
  segments,
  selectedId,
  onSelect,
  onTimingCommit,
}: Props) {
  const containerRef = useRef<HTMLDivElement | null>(null);
  const timelineRef = useRef<HTMLDivElement | null>(null);
  const instanceRef = useRef<Instance | null>(null);
  const [decoded, setDecoded] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Latest props for the WaveSurfer handlers. Kept out of the creation effect:
  // re-creating WaveSurfer refetches and re-decodes the whole file, so a
  // selection or segment change must never tear it down.
  const latest = useRef({ segments, selectedId, onSelect, onTimingCommit });
  latest.current = { segments, selectedId, onSelect, onTimingCommit };

  useEffect(() => {
    const container = containerRef.current;
    const timeline = timelineRef.current;
    if (!container || !timeline || !media || !sourceUrl) return;

    setError(null);
    setDecoded(false);
    const regions = RegionsPlugin.create();
    const timelinePlugin = TimelinePlugin.create({ container: timeline });

    const wave = WaveSurfer.create({
      container,
      media,
      url: sourceUrl,
      height: 88,
      waveColor: "rgba(142, 142, 147, .48)",
      progressColor: "rgba(250, 45, 72, .88)",
      cursorColor: "rgba(255,255,255,.72)",
      cursorWidth: 1,
      normalize: true,
      dragToSeek: true,
      autoScroll: true,
      autoCenter: false,
      plugins: [regions, timelinePlugin],
    });
    instanceRef.current = { wave, regions };

    const onDecoded = () => {
      drawRegions(regions, latest.current.segments, latest.current.selectedId);
      setDecoded(true);
    };
    const unDecode = wave.on("decode", onDecoded);
    const unReady = wave.on("ready", onDecoded);
    const unError = wave.on("error", (event) => {
      setError(event instanceof Error ? event.message : "Waveform unavailable");
    });
    const unClick = regions.on("region-clicked", (region, event) => {
      event.stopPropagation();
      const id = Number(region.id);
      const segment = latest.current.segments.find((row) => row.id === id);
      if (segment) latest.current.onSelect(segment);
    });
    const unUpdate = regions.on("region-updated", (region) => {
      const id = Number(region.id);
      const segment = latest.current.segments.find((row) => row.id === id);
      if (!segment) return;
      latest.current.onTimingCommit(
        segment,
        Math.round(region.start * 1000),
        Math.round(region.end * 1000),
      );
    });

    return () => {
      unDecode();
      unReady();
      unError();
      unClick();
      unUpdate();
      instanceRef.current = null;
      wave.destroy();
    };
  }, [media, sourceUrl]);

  // Segment edits and selection only redraw the regions.
  useEffect(() => {
    const instance = instanceRef.current;
    if (!instance || !decoded) return;
    drawRegions(instance.regions, segments, selectedId);
  }, [decoded, segments, selectedId]);

  return (
    <div className="waveform-editor">
      <div className="waveform-toolbar">
        <span>Waveform</span>
        <small>Drag a region to move · drag edges to retime</small>
      </div>
      <div ref={containerRef} className="waveform-canvas" />
      <div ref={timelineRef} className="waveform-timeline" />
      {error && <div className="waveform-error">{error}</div>}
    </div>
  );
}
