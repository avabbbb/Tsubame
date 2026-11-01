import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { SourceWork } from "./types";

// DLsite in Discover (docs/08 "DLsite account"). Public search needs no
// account. Sign-in happens on DLsite's own page in a separate window, so this
// panel never sees the password: it only learns whether a session exists,
// lists the purchased works and queues their official downloads.

interface SessionStatus {
  signed_in: boolean;
}

interface OwnedWork {
  rj_code: string;
  purchased_at: string | null;
}

const OWNED_ARCHIVE_ID = "dlsite-owned-archive";
const LOGIN_READY_EVENT = "dlsite-login-ready";
const DEST_KEY = "yuzuki.dlsite.destDir";
const SEARCH_LIMIT = 30;

function message(err: unknown) {
  return err instanceof Error ? err.message : String(err);
}

function WorkCard({
  work,
  extra,
  action,
  onOpen,
}: {
  work: SourceWork | null;
  extra: string;
  action?: React.ReactNode;
  onOpen?: () => void;
}) {
  const cover = work?.cover && work.cover.startsWith("http") ? work.cover : null;
  return (
    <div
      onClick={onOpen}
      title={onOpen ? "Show details" : undefined}
      style={{ display: "flex", flexDirection: "column", gap: 4, minWidth: 0, cursor: onOpen ? "pointer" : undefined }}
    >
      <div style={{ aspectRatio: "4 / 3", background: "var(--surface-2, #2222)", borderRadius: 6, overflow: "hidden" }}>
        {cover && (
          <img
            src={cover}
            alt=""
            loading="lazy"
            onError={(event) => {
              event.currentTarget.style.display = "none";
            }}
            style={{ width: "100%", height: "100%", objectFit: "cover" }}
          />
        )}
      </div>
      <strong style={{ fontSize: 13, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }} title={work?.title}>
        {work?.title ?? extra}
      </strong>
      <span style={{ fontSize: 12, opacity: 0.75, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>
        {[work?.circle, extra].filter(Boolean).join(" · ")}
      </span>
      {action && <div onClick={(event) => event.stopPropagation()}>{action}</div>}
    </div>
  );
}

const grid = { display: "grid", gridTemplateColumns: "repeat(auto-fill, minmax(160px, 1fr))", gap: 12 } as const;

export default function DlsiteAccount() {
  const [status, setStatus] = useState<SessionStatus | null>(null);
  const [loginOpen, setLoginOpen] = useState(false);
  const [owned, setOwned] = useState<OwnedWork[] | null>(null);
  const [meta, setMeta] = useState<Record<string, SourceWork>>({});
  const [query, setQuery] = useState("");
  const [results, setResults] = useState<SourceWork[] | null>(null);
  const [details, setDetails] = useState<SourceWork | null>(null);
  const [destDir, setDestDir] = useState(() => localStorage.getItem(DEST_KEY) ?? "");
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  const ownedCodes = useMemo(() => new Set((owned ?? []).map((work) => work.rj_code)), [owned]);

  const run = useCallback(async (label: string, task: () => Promise<void>) => {
    setBusy(label);
    setError(null);
    setNotice(null);
    try {
      await task();
    } catch (err) {
      setError(message(err));
    } finally {
      setBusy(null);
    }
  }, []);

  const finishLogin = useCallback(
    () =>
      run("finish", async () => {
        setStatus(await invoke<SessionStatus>("dlsite_login_finish"));
        setLoginOpen(false);
        setNotice("Signed in to DLsite.");
      }),
    [run],
  );

  useEffect(() => {
    void run("status", async () => setStatus(await invoke<SessionStatus>("dlsite_session_status")));
  }, [run]);

  useEffect(() => {
    const unlisten = listen(LOGIN_READY_EVENT, () => void finishLogin());
    return () => {
      void unlisten.then((stop) => stop());
    };
  }, [finishLogin]);

  // Titles and covers for purchased works come from the public product data
  // (source.get_work, cached), one work at a time so DLsite is not flooded.
  useEffect(() => {
    if (!owned) return;
    let stopped = false;
    void (async () => {
      for (const work of owned) {
        if (stopped) return;
        try {
          const full = await invoke<SourceWork>("source_get_work", {
            input: { source_id: "dlsite", remote_id: work.rj_code, refresh: false },
          });
          if (!stopped) setMeta((prev) => ({ ...prev, [work.rj_code]: full }));
        } catch {
          // A work DLsite no longer lists keeps its RJ code as its title.
        }
      }
    })();
    return () => {
      stopped = true;
    };
  }, [owned]);

  const openLogin = () =>
    run("login", async () => {
      await invoke("dlsite_login_open");
      setLoginOpen(true);
    });

  const logout = () =>
    run("logout", async () => {
      setStatus(await invoke<SessionStatus>("dlsite_logout"));
      setOwned(null);
    });

  const loadLibrary = () => run("library", async () => setOwned(await invoke<OwnedWork[]>("dlsite_library")));

  const search = () =>
    run("search", async () => {
      const text = query.trim();
      if (!text) throw new Error("Type a keyword or RJ code.");
      setResults(
        await invoke<SourceWork[]>("source_search", {
          input: { source_id: "dlsite", query: text, limit: SEARCH_LIMIT },
        }),
      );
    });

  // Search hits carry only list fields; source.get_work fills CV, tags and date.
  const openDetails = (rj: string) =>
    run("details", async () => {
      setDetails(
        await invoke<SourceWork>("source_get_work", {
          input: { source_id: "dlsite", remote_id: rj, refresh: false },
        }),
      );
    });

  const download = (rj: string) =>
    run(rj, async () => {
      const dir = destDir.trim();
      if (!dir) throw new Error("Enter an absolute download folder first.");
      localStorage.setItem(DEST_KEY, dir);
      await invoke("download_enqueue", {
        input: { source_id: "dlsite", remote_id: rj, asset_ids: [OWNED_ARCHIVE_ID], dest_dir: dir },
      });
      setNotice(rj + " queued in the download queue.");
    });

  const downloadButton = (rj: string) => (
    <button onClick={() => void download(rj)} disabled={busy !== null}>
      {busy === rj ? "Queueing…" : "Download"}
    </button>
  );

  return (
    <section className="content" style={{ padding: 16, minHeight: 0 }}>
      <h2>DLsite</h2>
      <form
        onSubmit={(event) => {
          event.preventDefault();
          void search();
        }}
        style={{ display: "flex", gap: 8, marginBottom: 12 }}
      >
        <input
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Search DLsite (keyword or RJ code)"
          style={{ flex: 1 }}
        />
        <button type="submit" disabled={busy !== null}>
          {busy === "search" ? "Searching…" : "Search"}
        </button>
      </form>

      {error && (
        <p role="alert" style={{ color: "var(--danger, #c33)" }}>
          {error}
        </p>
      )}
      {notice && <p role="status">{notice}</p>}

      {details && (
        <div style={{ display: "flex", gap: 16, margin: "12px 0", alignItems: "flex-start" }}>
          {details.cover && <img src={details.cover} alt="" style={{ width: 240, borderRadius: 6 }} />}
          <div style={{ minWidth: 0 }}>
            <h3 style={{ marginTop: 0 }}>{details.title}</h3>
            <p>{[details.circle, details.rj_code, details.release_date].filter(Boolean).join(" · ")}</p>
            {details.voice_actors.length > 0 && <p>CV: {details.voice_actors.map((actor) => actor.name_ja).join(", ")}</p>}
            {details.tags.length > 0 && <p style={{ opacity: 0.8 }}>{details.tags.join(" / ")}</p>}
            {details.languages.length > 0 && <p style={{ opacity: 0.8 }}>Languages: {details.languages.join(", ")}</p>}
            <button onClick={() => setDetails(null)}>Close</button>
          </div>
        </div>
      )}

      {results && (
        <>
          <h3>Search results ({results.length})</h3>
          {results.length === 0 ? (
            <p>Nothing found.</p>
          ) : (
            <div style={grid}>
              {results.map((work) => {
                const rj = work.rj_code ?? work.remote_id;
                return (
                  <WorkCard
                    key={work.remote_id}
                    work={work}
                    extra={rj}
                    onOpen={() => void openDetails(rj)}
                    action={status?.signed_in && ownedCodes.has(rj) ? downloadButton(rj) : undefined}
                  />
                );
              })}
            </div>
          )}
        </>
      )}

      <h3>Account</h3>
      <p style={{ opacity: 0.8 }}>
        Downloads only works this account bought, through DLsite's own download. Your password stays on
        DLsite's page; YUZUKI keeps the session in the OS keyring.
      </p>
      {status === null ? (
        <p>Checking session…</p>
      ) : status.signed_in ? (
        <div style={{ display: "flex", gap: 8, flexWrap: "wrap", alignItems: "center" }}>
          <button onClick={() => void loadLibrary()} disabled={busy !== null}>
            {busy === "library" ? "Loading…" : owned ? "Refresh purchases" : "Show purchases"}
          </button>
          <button onClick={() => void logout()} disabled={busy !== null}>
            Sign out
          </button>
          <label>
            Download folder{" "}
            <input value={destDir} onChange={(event) => setDestDir(event.target.value)} placeholder="D:\\ASMR" />
          </label>
        </div>
      ) : (
        <div style={{ display: "flex", gap: 8, flexWrap: "wrap" }}>
          <button onClick={() => void openLogin()} disabled={busy !== null}>
            Sign in to DLsite
          </button>
          {loginOpen && (
            <button onClick={() => void finishLogin()} disabled={busy !== null}>
              {busy === "finish" ? "Checking…" : "I've signed in"}
            </button>
          )}
        </div>
      )}
      {status?.signed_in && owned && (
        <>
          <h3>Purchased ({owned.length})</h3>
          {owned.length === 0 ? (
            <p>No purchases on this account.</p>
          ) : (
            <div style={grid}>
              {owned.map((work) => (
                <WorkCard
                  key={work.rj_code}
                  work={meta[work.rj_code] ?? null}
                  extra={work.rj_code}
                  onOpen={() => void openDetails(work.rj_code)}
                  action={downloadButton(work.rj_code)}
                />
              ))}
            </div>
          )}
        </>
      )}
    </section>
  );
}
