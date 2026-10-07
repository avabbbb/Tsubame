import { useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  AsrTranscriptionOutcome,
  CapabilityTarget,
  MediaItem,
  Segment,
} from "./types";

type Props = {
  media: MediaItem;
  onSegments: (segments: Segment[]) => void;
};

export default function AsrPanel({ media, onSegments }: Props) {
  const [targets, setTargets] = useState<CapabilityTarget[]>([]);
  const [providerId, setProviderId] = useState("");
  const [modelId, setModelId] = useState("");
  const [language, setLanguage] = useState("ja");
  const [prompt, setPrompt] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");

  const providers = useMemo(() => {
    const seen = new Map<string, CapabilityTarget>();
    for (const target of targets) {
      if (!seen.has(target.provider_id)) seen.set(target.provider_id, target);
    }
    return [...seen.values()];
  }, [targets]);

  const providerModels = useMemo(
    () => targets.filter((target) => target.provider_id === providerId),
    [providerId, targets],
  );

  const selectedTarget = useMemo(
    () =>
      targets.find(
        (target) =>
          target.provider_id === providerId && target.model_id === modelId,
      ) ?? null,
    [modelId, providerId, targets],
  );

  const load = async () => {
    const rows = await invoke<CapabilityTarget[]>("resolve_capability_targets", {
      capability: "speech.asr",
    });
    setTargets(rows);

    const preferred =
      rows.find(
        (target) =>
          target.provider_id === "local.faster-whisper" &&
          target.available &&
          target.provider_availability === "ready",
      ) ??
      rows.find(
        (target) =>
          target.available && target.provider_availability === "ready",
      ) ??
      rows[0];

    if (preferred) {
      const currentProviderIsValid = rows.some(
        (target) => target.provider_id === providerId,
      );
      const nextProviderId = currentProviderIsValid
        ? providerId
        : preferred.provider_id;
      setProviderId(nextProviderId);

      const currentModelIsValid = rows.some(
        (target) =>
          target.provider_id === nextProviderId &&
          target.model_id === modelId,
      );
      if (!currentModelIsValid) {
        const firstForProvider =
          rows.find(
            (target) =>
              target.provider_id === nextProviderId &&
              target.available &&
              target.provider_availability === "ready",
          ) ??
          rows.find((target) => target.provider_id === nextProviderId);
        setModelId(firstForProvider?.model_id ?? "");
      }
    }
  };

  useEffect(() => {
    void load().catch((error) => setMessage(String(error)));
    // Provider/runtime state may change outside this surface; refresh per track.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [media.id]);

  useEffect(() => {
    if (!providerModels.length) return;
    if (!providerModels.some((target) => target.model_id === modelId)) {
      setModelId(
        providerModels.find(
          (target) =>
            target.available && target.provider_availability === "ready",
        )?.model_id ?? providerModels[0].model_id,
      );
    }
  }, [modelId, providerModels]);

  const run = async () => {
    if (!selectedTarget) {
      setMessage("Choose a Provider and model first.");
      return;
    }
    if (
      selectedTarget.provider_availability !== "ready" ||
      !selectedTarget.available
    ) {
      setMessage(
        selectedTarget.provider_message ||
          "The selected Provider/model is not ready.",
      );
      return;
    }

    setBusy(true);
    setMessage(
      "Transcribing… existing Segments are replaced only after a valid result returns.",
    );
    try {
      const outcome = await invoke<AsrTranscriptionOutcome>("transcribe_media", {
        input: {
          media_id: media.id,
          provider_id: selectedTarget.provider_id,
          model_id: selectedTarget.model_id,
          language: language || null,
          prompt: prompt.trim() || null,
        },
      });
      onSegments(outcome.segments);
      setMessage(
        selectedTarget.provider_name +
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
          <strong>Provider-based ASR</strong>
          <small>
            Local runtimes, native OS models, local servers and paid APIs resolve
            through the same speech.asr capability.
          </small>
        </div>
        <button className="secondary" disabled={busy} onClick={() => void load()}>
          Refresh
        </button>
      </div>

      <div className="asr-run-card">
        <div className="asr-run-grid">
          <label>
            Provider
            <select
              value={providerId}
              onChange={(event) => {
                const next = event.target.value;
                setProviderId(next);
                const first = targets.find(
                  (target) => target.provider_id === next,
                );
                setModelId(first?.model_id ?? "");
                setMessage("");
              }}
            >
              {!providers.length && <option value="">No speech.asr Provider</option>}
              {providers.map((provider) => (
                <option key={provider.provider_id} value={provider.provider_id}>
                  {provider.provider_name} · {provider.execution.replace("_", " ")}
                </option>
              ))}
            </select>
          </label>

          <label>
            Model
            <select
              value={modelId}
              onChange={(event) => setModelId(event.target.value)}
            >
              {providerModels.map((target) => (
                <option key={target.model_id} value={target.model_id}>
                  {target.display_name || target.model_id}
                  {target.available ? "" : " · not ready"}
                </option>
              ))}
            </select>
          </label>

          <label>
            Language
            <select
              value={language}
              onChange={(event) => setLanguage(event.target.value)}
            >
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

        {selectedTarget && (
          <div className="asr-provider-summary">
            <span>
              <strong>{selectedTarget.provider_name}</strong>
              <small>
                {selectedTarget.execution.replace("_", " ")} ·{" "}
                {selectedTarget.provider_kind}
              </small>
            </span>
            <em data-state={selectedTarget.provider_availability}>
              {selectedTarget.provider_availability}
            </em>
            <p>{selectedTarget.provider_message}</p>
          </div>
        )}

        <div className="asr-run-actions">
          <span>
            Provider + Model are resolved by capability; adapter details stay
            internal to the Provider.
          </span>
          <button
            className="primary"
            disabled={
              busy ||
              !selectedTarget ||
              selectedTarget.provider_availability !== "ready" ||
              !selectedTarget.available ||
              media.media_type !== "audio"
            }
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
    </section>
  );
}
