import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

export type DictKind = "jmdict" | "ecdict";

export interface DictEntry {
  source: string;
  headword: string;
  reading: string;
  gloss: string;
}

export interface DictSource {
  source: string;
  entries: number;
  imported_at: string;
  attribution: string;
}

export interface DictLookupResult {
  matched: string;
  entries: DictEntry[];
}

const KINDS: { kind: DictKind; label: string; ext: string; hint: string }[] = [
  { kind: "jmdict", label: "JMdict (jmdict-simplified JSON)", ext: "json", hint: "Japanese → English" },
  { kind: "ecdict", label: "ECDICT (CSV)", ext: "csv", hint: "English → Chinese" },
];

export async function lookupWord(query: string): Promise<DictLookupResult | null> {
  const trimmed = query.trim();
  if (!trimmed) return null;
  return invoke<DictLookupResult | null>("dict_lookup", { query: trimmed });
}

export function DictResults({ result }: { result: DictLookupResult | null }) {
  if (!result) return <p className="muted">No match.</p>;
  return (
    <div className="dict-results">
      <p className="muted">Matched: {result.matched}</p>
      <ul>
        {result.entries.map((entry, idx) => (
          <li key={entry.source + "-" + entry.headword + "-" + idx}>
            <strong>{entry.headword}</strong>
            {entry.reading && entry.reading !== entry.headword ? <span> 【{entry.reading}】</span> : null}
            <span className="muted"> · {entry.source}</span>
            <div>{entry.gloss}</div>
          </li>
        ))}
      </ul>
    </div>
  );
}

export default function DictionaryPanel({ initialQuery = "" }: { initialQuery?: string }) {
  const [query, setQuery] = useState(initialQuery);
  const [result, setResult] = useState<DictLookupResult | null | undefined>(undefined);
  const [sources, setSources] = useState<DictSource[]>([]);
  const [busy, setBusy] = useState<DictKind | null>(null);
  const [error, setError] = useState<string | null>(null);

  const refresh = useCallback(async () => {
    try {
      setSources(await invoke<DictSource[]>("dict_sources"));
    } catch (err) {
      setError(String(err));
    }
  }, []);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  useEffect(() => {
    if (initialQuery) setQuery(initialQuery);
  }, [initialQuery]);

  const runLookup = async () => {
    setError(null);
    try {
      setResult(await lookupWord(query));
    } catch (err) {
      setError(String(err));
    }
  };

  const importKind = async (kind: DictKind, ext: string) => {
    setError(null);
    const picked = await open({ multiple: false, filters: [{ name: kind, extensions: [ext] }] });
    if (typeof picked !== "string") return;
    setBusy(kind);
    try {
      await invoke<DictSource>("dict_import", { kind, path: picked });
      await refresh();
    } catch (err) {
      setError(String(err));
    } finally {
      setBusy(null);
    }
  };

  const removeKind = async (kind: string) => {
    setError(null);
    try {
      await invoke("dict_remove", { kind });
      await refresh();
    } catch (err) {
      setError(String(err));
    }
  };

  return (
    <section className="card dictionary-panel">
      <h2>Offline dictionary</h2>
      <p className="muted">
        YUZUKI does not ship dictionary data. Import a dictionary file you downloaded yourself; it is stored locally in
        dictionary.db and never uploaded.
      </p>

      <form
        className="dict-lookup"
        onSubmit={(event) => {
          event.preventDefault();
          void runLookup();
        }}
      >
        <input
          value={query}
          placeholder="Look up a word…"
          onChange={(event) => setQuery(event.target.value)}
          aria-label="Dictionary query"
        />
        <button type="submit" disabled={!query.trim() || sources.length === 0}>
          Look up
        </button>
      </form>
      {sources.length === 0 ? <p className="muted">Import a dictionary to enable lookup.</p> : null}
      {result !== undefined ? <DictResults result={result} /> : null}

      <h3>Sources</h3>
      <ul className="dict-sources">
        {KINDS.map(({ kind, label, ext, hint }) => {
          const installed = sources.find((s) => s.source === kind);
          return (
            <li key={kind}>
              <div>
                <strong>{label}</strong> <span className="muted">{hint}</span>
              </div>
              {installed ? (
                <div className="muted">
                  {installed.entries.toLocaleString()} entries · imported {installed.imported_at}
                  <div className="dict-attribution">{installed.attribution}</div>
                </div>
              ) : null}
              <div className="row">
                <button disabled={busy !== null} onClick={() => void importKind(kind, ext)}>
                  {busy === kind ? "Importing…" : installed ? "Replace" : "Import ." + ext}
                </button>
                {installed ? (
                  <button disabled={busy !== null} onClick={() => void removeKind(kind)}>
                    Remove
                  </button>
                ) : null}
              </div>
            </li>
          );
        })}
      </ul>
      {error ? <p className="error">{error}</p> : null}
    </section>
  );
}
