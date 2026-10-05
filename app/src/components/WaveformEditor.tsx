import { useEffect, useRef } from "react";
import WaveSurfer from "wavesurfer.js";
import RegionsPlugin from "wavesurfer.js/plugins/regions";
import TimelinePlugin from "wavesurfer.js/plugins/timeline";
import type { Segment } from "../types";

type Props = {
  media: HTMLMediaElement | null;
  sourceUrl: string;
  segments: Segment[];
  selectedId: number | null;
  onSelect: (id: number) => void;
  onTimingCommit: (
    id: number,
    startMs: number,
    endMs: number,
    expectedRevision: number,
  ) => void;
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
  const wavesurferRef = useRef<WaveSurfer | null>(null);
  const regionsRef = useRef<ReturnType<typeof RegionsPlugin.create> | null>(null);
  const segmentsRef = useRef(segments);
  const onSelectRef = useRef(onSelect);
  const onTimingCommitRef = useRef(onTimingCommit);

  segmentsRef.current = segments;
  onSelectRef.current = onSelect;
  onTimingCommitRef.current = onTimingCommit;

  useEffect(() => {
    if (!containerRef.current || !media || !sourceUrl) return;

    const regions = RegionsPlugin.create();
    const timeline = TimelinePlugin.create({ height: 18 });

    const wavesurfer = WaveSurfer.create({
      container: containerRef.current,
      media,
      url: sourceUrl,
      height: 84,
      waveColor: "rgba(130,130,138,.45)",
      progressColor: "rgba(250,45,72,.75)",
      cursorColor: "rgba(255,255,255,.85)",
      cursorWidth: 1,
      normalize: true,
      dragToSeek: true,
      minPxPerSec: 18,
      autoScroll: true,
      autoCenter: false,
      plugins: [regions, timeline],
    });

    wavesurferRef.current = wavesurfer;
    regionsRef.current = regions;

    const clicked = regions.on("region-clicked", (region, event) => {
      event.stopPropagation();
      const id = Number(region.id);
      if (Number.isFinite(id)) onSelectRef.current(id);
    });

    const updated = regions.on("region-updated", (region) => {
      const id = Number(region.id);
      const segment = segmentsRef.current.find((row) => row.id === id);
      if (!segment) return;
      onTimingCommitRef.current(
        id,
        Math.round(region.start * 1000),
        Math.round(region.end * 1000),
        segment.revision,
      );
    });

    return () => {
      clicked();
      updated();
      wavesurfer.destroy();
      wavesurferRef.current = null;
      regionsRef.current = null;
    };
  }, [media, sourceUrl]);

  useEffect(() => {
    const regions = regionsRef.current;
    const wavesurfer = wavesurferRef.current;
    if (!regions || !wavesurfer || wavesurfer.getDuration() <= 0) return;

    regions.clearRegions();
    for (const segment of segments) {
      regions.addRegion({
        id: String(segment.id),
        start: segment.start_ms / 1000,
        end: segment.end_ms / 1000,
        drag: true,
        resize: true,
        minLength: 0.08,
        color:
          segment.id === selectedId
            ? "rgba(250,45,72,.18)"
            : "rgba(130,130,138,.10)",
      });
    }
  }, [segments, selectedId]);

  useEffect(() => {
    const wavesurfer = wavesurferRef.current;
    const regions = regionsRef.current;
    if (!wavesurfer || !regions) return;

    const render = () => {
      regions.clearRegions();
      for (const segment of segmentsRef.current) {
        regions.addRegion({
          id: String(segment.id),
          start: segment.start_ms / 1000,
          end: segment.end_ms / 1000,
          drag: true,
          resize: true,
          minLength: 0.08,
          color:
            segment.id === selectedId
              ? "rgba(250,45,72,.18)"
              : "rgba(130,130,138,.10)",
        });
      }
    };

    const unsubscribe = wavesurfer.on("ready", render);
    return unsubscribe;
  }, [selectedId]);

  return (
    <div className="waveform-shell">
      <div className="waveform-head">
        <span>WAVEFORM</span>
        <small>Drag a region or its handles to edit timing</small>
      </div>
      <div ref={containerRef} className="waveform-canvas" />
    </div>
  );
}
