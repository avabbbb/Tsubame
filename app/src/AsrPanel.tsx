import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  AsrEngineDescriptor,
  AsrTranscriptionOutcome,
  MediaItem,
  ProviderConfig,
  ProviderModel,
  Segment,
} from "./types";

type Props = {
  media: MediaItem;
  onSegments: (segments: Segment[]) => void;
};

export default function AsrPanel({ media, onSegments }: Props) {
  const [engines, setEngines] = useState<AsrEngineDescriptor[]>([]);
  const [providers, setProviders] = useState<ProviderConfig[]>([]);
  const [remoteModels, setRemoteModels] = useState<ProviderModel[]>([]);
  const [engineId, setEngineId] = useState("");
  const [remoteChoice, setRemoteChoice] = useState("");
  const [modelId, setModelId] = useState("");
  const [language, setLanguage] = useState("ja");
  const [prompt, setPrompt] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");

  const selectedEngine = useMemo(
    () => engines.find((engine) => engine.id === engineId) ?? null,
    [engineId, engines],
  );

  const providerNames = useMemo(
    () => new Map(providers.map((provider) => [provider.id, provider.name])),
    [providers],
  );

  const load = async () => {
    const [engineRows, providerRows, modelRows] = await Promise.all([
      invoke<AsrEngineDescriptor[]>("list_asr_engines"),
      invoke<ProviderConfig[]>("list_providers"),
      invoke<ProviderModel[]>("resolve_capability", { capability: "speech.asr" }),
    ]);
    setEngines(engineRows);
    setProviders(providerRows);
    setRemoteModels(modelRows);

    if (!engineId) {
      const firstReady =
        engineRows.find((engine) => engine.id === "faster-whisper" && engine.availability === "ready") ??
        engineRows.find((engine) => engine.availability === "ready");
      if (firstReady) {
        setEngineId(firstReady.id);
        setModelId(firstReady.default_model);
      }
    }

    if (!remoteChoice && modelRows[0]) {
      setRemoteChoice(modelRows[0].provider_id + "\t" + modelRows[0].model_id);
    }
  };

  useEffect(() => {
    void load().catch((error) => setMessage(String(error)));
    // Provider/runtime state may change outside this surface; refresh per track.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [media.id]);

  useEffect(() => {
    if (!selectedEngine || selectedEngine.id === "remote-openai-compatible") return;
    setModelId(selectedEngine.default_model);
  }, [selectedEngine]);

  const run = async () => {
    if (!selectedEngine || selectedEngine.availability !== "ready") return;

    let providerId: string | null = null;
    let resolvedModelId = modelId.trim() || selectedEngine.default_model;

    if (selectedEngine.id === "remote-openai-compatible") {
      const [provider, model] = remoteChoice.split("\t");
      if (!provider || !model) {
        setMessage("Choose a remote ASR model first.");
        return;
      }
      providerId = provider;
      resolvedModelId = model;
    }

    setBusy(true);
    setMessage("Transcribing… existing Segments are replaced only after a valid result returns.");
    try {
      const outcome = await invoke<AsrTranscriptionOutcome>("transcribe_media", {
        input: {
          media_id: media.id,
          engine_id: selectedEngine.id,
          provider_id: providerId,
          model_id: resolvedModelId || null,
          language: language || null,
          prompt: prompt.trim() || null,
        },
      });
      onSegments(outcome.segments);
      const engine = engines.find((row) => row.id === outcome.result.engine_id);
      setMessage(
        (engine?.name ?? outcome.result.engine_id) +
          " · " +
          outcome.result.model_id +
          " · " +
          outcome.segments.length +
          " segments",
      );
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  };

  return (
    <section className="asr-panel">
      <div className="asr-panel-head">
        <div>
          <p className="section-label">TRANSCRIBE</p>
          <strong>Modular ASR</strong>
          <small>
            Every engine returns candidate Segments. Only Tsubame writes SQLite.
          </small>
        </div>
        <button className="secondary" disabled={busy} onClick={() => void load()}>
          Refresh
        </button>
      </div>

      <div className="asr-engine-grid">
        {engines.map((engine) => (
          <button
            key={engine.id}
            className={"asr-engine-card " + (engineId === engine.id ? "active" : "")}
            onClick={() => {
              setEngineId(engine.id);
              setMessage("");
            }}
          >
            <span>
              <strong>{engine.name}</strong>
              <em data-state={engine.availability}>{engine.availability}</em>
            </span>
            <small>{engine.message}</small>
          </button>
        ))}
      </div>

      {selectedEngine && (
        <div className="asr-run-card">
          {selectedEngine.id === "remote-openai-compatible" ? (
            <label>
              Provider / model
              <select
                value={remoteChoice}
                onChange={(event) => setRemoteChoice(event.target.value)}
              >
                {!remoteModels.length && (
                  <option value="">No remote speech.asr model</option>
                )}
                {remoteModels.map((model) => (
                  <option
                    key={model.provider_id + ":" + model.model_id}
                    value={model.provider_id + "\t" + model.model_id}
                  >
                    {(providerNames.get(model.provider_id) ?? model.provider_id) + " · " + model.model_id}
                  </option>
                ))}
              </select>
            </label>
          ) : (
            <label>
              Model
              <input
                value={modelId}
                onChange={(event) => setModelId(event.target.value)}
                placeholder={selectedEngine.default_model || "runtime model ID/path"}
                spellCheck={false}
              />
            </label>
          )}

          <div className="asr-run-grid">
            <label>
              Language
              <select value={language} onChange={(event) => setLanguage(event.target.value)}>
                <option value="ja">Japanese · ja</option>
                <option value="auto">Auto detect</option>
                <option value="zh">Chinese · zh</option>
                <option value="en">English · en</option>
                <option value="ko">Korean · ko</option>
                <option value="yue">Cantonese · yue</option>
              </select>
            </label>
            <label>
              Prompt / glossary hint
              <input
                value={prompt}
                onChange={(event) => setPrompt(event.target.value)}
                placeholder="Optional names / terminology"
              />
            </label>
          </div>

          <div className="asr-run-actions">
            <span>
              {selectedEngine.supports_segment_timestamps
                ? "Segment timestamps supported"
                : "Whole-track transcript only"}
            </span>
            <button
              className="primary"
              disabled={busy || selectedEngine.availability !== "ready" || media.media_type !== "audio"}
              onClick={() => void run()}
            >
              {busy ? "Transcribing…" : "Transcribe track"}
            </button>
          </div>

          {media.media_type !== "audio" && (
            <div className="asr-message">
              Video audio extraction is intentionally deferred to Runtime Bootstrap.
            </div>
          )}
          {message && <div className="asr-message">{message}</div>}
        </div>
      )}
    </section>
  );
}
