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
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    const container = containerRef.current;
    const timeline = timelineRef.current;
    if (!container || !timeline || !media || !sourceUrl) return;

    setError(null);
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

    const renderRegions = () => {
      regions.clearRegions();
      for (const segment of segments) {
        const selected = segment.id === selectedId;
        regions.addRegion({
          id: String(segment.id),
          start: segment.start_ms / 1000,
          end: segment.end_ms / 1000,
          color: selected
            ? "rgba(250, 45, 72, .22)"
            : "rgba(142, 142, 147, .10)",
          drag: true,
          resize: true,
          minLength: 0.05,
          content: String(segment.ordinal + 1),
        });
      }
    };

    const unDecode = wave.on("decode", renderRegions);
    const unReady = wave.on("ready", renderRegions);
    const unError = wave.on("error", (event) => {
      setError(event instanceof Error ? event.message : "Waveform unavailable");
    });
    const unClick = regions.on("region-clicked", (region, event) => {
      event.stopPropagation();
      const id = Number(region.id);
      const segment = segments.find((row) => row.id === id);
      if (segment) onSelect(segment);
    });
    const unUpdate = regions.on("region-updated", (region) => {
      const id = Number(region.id);
      const segment = segments.find((row) => row.id === id);
      if (!segment) return;
      onTimingCommit(
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
      wave.destroy();
    };
  }, [media, onSelect, onTimingCommit, segments, selectedId, sourceUrl]);

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
