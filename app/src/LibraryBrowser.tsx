import { useMemo, useState } from "react";
import { convertFileSrc, invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import type { AssetKind, DownloadQueued, SourceWork } from "./types";

// Library browser (PR #14, docs/08): works from lawful sources, asked
// "what does this work contain?", with chosen files sent to download.enqueue.
// Only the local adapter feeds it today.

const KINDS: AssetKind[] = ["audio", "video", "subtitle", "text", "image", "archive", "metadata"];
const DEFAULT_KINDS: AssetKind[] = ["audio", "video", "subtitle"];

function formatBytes(size: number | null) {
  if (size === null) return "";
  if (size < 1024) return size + " B";
  if (size < 1024 * 1024) return (size / 1024).toFixed(1) + " KB";
  if (size < 1024 * 1024 * 1024) return (size / 1024 / 1024).toFixed(1) + " MB";
  return (size / 1024 / 1024 / 1024).toFixed(2) + " GB";
}

function coverSrc(cover: string | null) {
  if (!cover) return null;
  return /^https?:\/\//.test(cover) ? cover : convertFileSrc(cover);
}

function matches(work: SourceWork, query: string) {
  const fields = [
    work.title,
    work.circle ?? "",
    work.rj_code ?? "",
    ...work.tags,
    ...work.voice_actors.flatMap((actor) => [actor.name_ja, ...actor.aliases]),
  ];
  return fields.some((field) => field.toLowerCase().includes(query));
}

const grid = {
  display: "grid",
  gridTemplateColumns: "repeat(auto-fill, minmax(150px, 1fr))",
  gap: 12,
} as const;

export default function LibraryBrowser({ onQueued }: { onQueued: () => void }) {
  const [works, setWorks] = useState<SourceWork[]>([]);
  const [activeId, setActiveId] = useState<string | null>(null);
  const [query, setQuery] = useState("");
  const [kinds, setKinds] = useState<Set<AssetKind>>(new Set(DEFAULT_KINDS));
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [brokenCovers, setBrokenCovers] = useState<Set<string>>(new Set());

  const active = works.find((work) => work.remote_id === activeId) ?? null;
  const visibleWorks = useMemo(() => {
    const q = query.trim().toLowerCase();
    return q ? works.filter((work) => matches(work, q)) : works;
  }, [works, query]);
  const visibleAssets = useMemo(
    () => (active ? active.assets.filter((asset) => kinds.has(asset.kind)) : []),
    [active, kinds],
  );
  const counts = useMemo(() => {
    const map = new Map<AssetKind, number>();
    for (const asset of active?.assets ?? []) map.set(asset.kind, (map.get(asset.kind) ?? 0) + 1);
    return map;
  }, [active]);

  const select = (work: SourceWork) => {
    setActiveId(work.remote_id);
    setSelected(
      new Set(work.assets.filter((asset) => DEFAULT_KINDS.includes(asset.kind)).map((asset) => asset.id)),
    );
  };

  const openFolder = async () => {
    const picked = await open({ directory: true, multiple: false });
    if (typeof picked !== "string") return;
    setBusy(true);
    setMessage(null);
    try {
      const work = await invoke<SourceWork>("source_get_work", {
        input: { source_id: "local", remote_id: picked, refresh: true },
      });
      setWorks((prev) => [work, ...prev.filter((w) => w.remote_id !== work.remote_id)]);
      select(work);
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  };

  const toggleKind = (kind: AssetKind) =>
    setKinds((prev) => {
      const next = new Set(prev);
      if (next.has(kind)) next.delete(kind);
      else next.add(kind);
      return next;
    });

  const toggleAsset = (id: string) =>
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) next.delete(id);
      else next.add(id);
      return next;
    });

  const enqueue = async () => {
    if (!active || !selected.size) return;
    const dest = await open({ directory: true, multiple: false, title: "Download to folder" });
    if (typeof dest !== "string") return;
    setBusy(true);
    setMessage(null);
    try {
      const queued = await invoke<DownloadQueued>("download_enqueue", {
        input: {
          source_id: active.source_id,
          remote_id: active.remote_id,
          asset_ids: [...selected],
          dest_dir: dest,
          import: true,
        },
      });
      setMessage("Queued " + queued.downloads.length + " file(s).");
      onQueued();
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="content" style={{ overflowY: "auto", minHeight: 0, padding: 16 }}>
      <p className="eyebrow">DISCOVER</p>
      <h2>Library browser</h2>
      <div style={{ display: "flex", gap: 8, alignItems: "center", margin: "8px 0 16px" }}>
        <button onClick={() => void openFolder()} disabled={busy}>
          Open folder…
        </button>
        <input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Filter by title, circle, CV, tag or RJ code"
          style={{ flex: 1 }}
        />
      </div>
      {message && <p role="status">{message}</p>}

      {!works.length && (
        <p>
          Open a folder of your own files. Subtitles (vtt/srt/lrc), scripts and images in the same
          folder come along with the audio.
        </p>
      )}

      <div style={grid}>
        {visibleWorks.map((work) => {
          const src = brokenCovers.has(work.remote_id) ? null : coverSrc(work.cover);
          return (
            <button
              key={work.remote_id}
              className={work.remote_id === activeId ? "active" : "secondary"}
              onClick={() => select(work)}
              style={{ display: "flex", flexDirection: "column", alignItems: "stretch", padding: 8, textAlign: "left" }}
              title={work.remote_id}
            >
              <div style={{ aspectRatio: "1", background: "rgba(127,127,127,0.15)", borderRadius: 6, overflow: "hidden", display: "grid", placeItems: "center" }}>
                {src ? (
                  <img
                    src={src}
                    alt=""
                    style={{ width: "100%", height: "100%", objectFit: "cover" }}
                    onError={() => setBrokenCovers((prev) => new Set(prev).add(work.remote_id))}
                  />
                ) : (
                  <span style={{ fontSize: 32, opacity: 0.4 }}>♪</span>
                )}
              </div>
              <strong style={{ marginTop: 6, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{work.title}</strong>
              <small>{work.circle ?? work.rj_code ?? work.assets.length + " files"}</small>
            </button>
          );
        })}
      </div>

      {active && (
        <div style={{ marginTop: 24 }}>
          <h3 style={{ marginBottom: 4 }}>{active.title}</h3>
          <p style={{ margin: "0 0 8px" }}>
            {[active.rj_code, active.circle, active.voice_actors.map((actor) => actor.name_ja).join(" / "), active.release_date]
              .filter(Boolean)
              .join(" · ")}
          </p>
          {active.tags.length > 0 && <p style={{ margin: "0 0 8px", opacity: 0.8 }}>{active.tags.join(", ")}</p>}

          <div style={{ display: "flex", flexWrap: "wrap", gap: 12, margin: "8px 0" }}>
            {KINDS.filter((kind) => counts.has(kind)).map((kind) => (
              <label key={kind}>
                <input type="checkbox" checked={kinds.has(kind)} onChange={() => toggleKind(kind)} /> {kind} ({counts.get(kind)})
              </label>
            ))}
          </div>

          <div style={{ display: "flex", gap: 8, margin: "8px 0" }}>
            <button
              className="secondary"
              onClick={() => setSelected((prev) => new Set([...prev, ...visibleAssets.map((asset) => asset.id)]))}
            >
              Select shown
            </button>
            <button className="secondary" onClick={() => setSelected(new Set())}>
              Clear
            </button>
            <button onClick={() => void enqueue()} disabled={busy || !selected.size}>
              Download {selected.size} to folder…
            </button>
          </div>

          <ul style={{ listStyle: "none", padding: 0, margin: 0 }}>
            {visibleAssets.map((asset) => (
              <li key={asset.id} style={{ display: "flex", gap: 8, alignItems: "center", padding: "4px 0" }}>
                <input
                  type="checkbox"
                  checked={selected.has(asset.id)}
                  onChange={() => toggleAsset(asset.id)}
                  aria-label={asset.name}
                />
                <span style={{ flex: 1, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={asset.location}>
                  {asset.name}
                </span>
                <small style={{ width: 70 }}>{asset.kind}</small>
                <small style={{ width: 80, textAlign: "right" }}>{formatBytes(asset.size)}</small>
              </li>
            ))}
          </ul>
        </div>
      )}
    </section>
  );
}
