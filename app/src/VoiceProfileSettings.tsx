import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  CapabilityTarget,
  VoiceKind,
  VoiceProfile,
  VoiceProfileInput,
} from "./types";
import { userMessage } from "./errors";

const EMPTY: VoiceProfileInput = {
  id: null,
  name: "",
  kind: "design",
  provider_id: "",
  model_id: "",
  preset_voice: "",
  design_prompt: "",
  language: "zh-Hans",
  params_json: "",
};

function toInput(profile: VoiceProfile): VoiceProfileInput {
  return {
    id: profile.id,
    name: profile.name,
    kind: profile.kind,
    provider_id: profile.provider_id,
    model_id: profile.model_id,
    preset_voice: profile.preset_voice,
    design_prompt: profile.design_prompt,
    language: profile.language,
    params_json: profile.params_json === "{}" ? "" : profile.params_json,
  };
}

/** Settings section: reusable dub voices (preset or text-designed). */
export default function VoiceProfileSettings() {
  const [profiles, setProfiles] = useState<VoiceProfile[]>([]);
  const [targets, setTargets] = useState<CapabilityTarget[]>([]);
  const [draft, setDraft] = useState<VoiceProfileInput>(EMPTY);
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);

  const load = useCallback(async () => {
    const [rows, ttsTargets] = await Promise.all([
      invoke<VoiceProfile[]>("list_voice_profiles"),
      invoke<CapabilityTarget[]>("resolve_capability_targets", {
        capability: "speech.tts",
      }),
    ]);
    setProfiles(rows);
    setTargets(ttsTargets);
  }, []);

  useEffect(() => {
    void load().catch((error) => setMessage(userMessage(error)));
  }, [load]);

  const targetValue = draft.provider_id
    ? `${draft.provider_id}\u0000${draft.model_id}`
    : "";
  const knownTarget = useMemo(
    () =>
      targets.some(
        (t) => t.provider_id === draft.provider_id && t.model_id === draft.model_id,
      ),
    [draft.model_id, draft.provider_id, targets],
  );

  function update<K extends keyof VoiceProfileInput>(
    key: K,
    value: VoiceProfileInput[K],
  ) {
    setDraft((current) => ({ ...current, [key]: value }));
  }

  async function save() {
    setBusy(true);
    setMessage("");
    try {
      const saved = await invoke<VoiceProfile>("save_voice_profile", {
        input: {
          ...draft,
          design_prompt: draft.kind === "design" ? draft.design_prompt : null,
        },
      });
      await load();
      setDraft(toInput(saved));
      setMessage(`Saved voice “${saved.name}”.`);
    } catch (error) {
      setMessage(userMessage(error));
    } finally {
      setBusy(false);
    }
  }

  async function remove() {
    if (!draft.id) return;
    setBusy(true);
    try {
      await invoke<boolean>("delete_voice_profile", { id: draft.id });
      setDraft(EMPTY);
      await load();
      setMessage("Voice deleted. Clips already synthesized stay in the cache.");
    } catch (error) {
      setMessage(userMessage(error));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="provider-layout voice-profile-settings">
      <aside className="provider-list">
        <p className="section-label">DUB VOICES</p>
        {profiles.map((profile) => (
          <button
            key={profile.id}
            className={`provider-list-row ${draft.id === profile.id ? "active" : ""}`}
            onClick={() => setDraft(toInput(profile))}
          >
            <span>
              <strong>{profile.name}</strong>
              <small>
                {profile.kind} · {profile.model_id}
              </small>
            </span>
          </button>
        ))}
        <button className="provider-list-row" onClick={() => setDraft(EMPTY)}>
          <span>
            <strong>＋ New voice</strong>
            <small>preset or design</small>
          </span>
        </button>
      </aside>

      <div className="provider-editor">
        <div className="provider-form-card">
          <div className="provider-form-title">
            <div>
              <p className="section-label">VOICE PROFILE</p>
              <h3>{draft.id ? draft.name : "New voice"}</h3>
            </div>
            <span className="provider-id-chip">speech.tts</span>
          </div>
          <p className="inspector-help">
            Voices are Provider presets or text-described designs. Cloning a real
            performer’s voice is not supported.
          </p>

          <div className="provider-form-grid">
            <label>
              Name
              <input
                value={draft.name}
                onChange={(event) => update("name", event.target.value)}
                placeholder="Soft narrator"
              />
            </label>
            <label>
              Kind
              <select
                value={draft.kind}
                onChange={(event) => update("kind", event.target.value as VoiceKind)}
              >
                <option value="design">Design · text description</option>
                <option value="preset">Preset · Provider voice</option>
              </select>
            </label>
            <label className="provider-span-two">
              Provider · Model (speech.tts)
              <select
                value={targetValue}
                onChange={(event) => {
                  const [providerId, modelId] = event.target.value.split("\u0000");
                  setDraft((current) => ({
                    ...current,
                    provider_id: providerId ?? "",
                    model_id: modelId ?? "",
                  }));
                }}
              >
                <option value="">Choose a model that resolves speech.tts…</option>
                {!knownTarget && draft.provider_id && (
                  <option value={targetValue}>
                    {draft.provider_id} · {draft.model_id} (no longer resolves)
                  </option>
                )}
                {targets.map((target) => (
                  <option
                    key={`${target.provider_id}/${target.model_id}`}
                    value={`${target.provider_id}\u0000${target.model_id}`}
                  >
                    {target.provider_name} · {target.display_name || target.model_id}
                  </option>
                ))}
              </select>
            </label>
            <label>
              {draft.kind === "preset" ? "Preset voice" : "Base voice (optional)"}
              <input
                value={draft.preset_voice ?? ""}
                onChange={(event) => update("preset_voice", event.target.value)}
                placeholder="e.g. alloy"
                spellCheck={false}
              />
            </label>
            <label>
              Language
              <select
                value={draft.language}
                onChange={(event) => update("language", event.target.value)}
              >
                <option value="zh-Hans">中文（简体）· zh-Hans</option>
                <option value="zh-Hant">中文（繁體）· zh-Hant</option>
                <option value="en">English · en</option>
                <option value="ja">日本語 · ja</option>
                <option value="">Unspecified</option>
              </select>
            </label>
            {draft.kind === "design" && (
              <label className="provider-span-two">
                Design prompt
                <textarea
                  value={draft.design_prompt ?? ""}
                  onChange={(event) => update("design_prompt", event.target.value)}
                  placeholder="Gentle, natural young female voice; calm pace; close whisper."
                />
              </label>
            )}
            <label className="provider-span-two">
              Extra params (JSON, optional)
              <input
                value={draft.params_json ?? ""}
                onChange={(event) => update("params_json", event.target.value)}
                placeholder='{"speed": 1.0}'
                spellCheck={false}
              />
            </label>
          </div>

          <div className="provider-form-actions">
            <span />
            <div>
              {draft.id && (
                <button className="secondary" disabled={busy} onClick={() => void remove()}>
                  Delete
                </button>
              )}
              <button className="primary" disabled={busy} onClick={() => void save()}>
                {busy ? "Working…" : "Save voice"}
              </button>
            </div>
          </div>
          {!targets.length && (
            <p className="inspector-help">
              No model resolves speech.tts yet. Refresh a Provider’s models or mark a
              model’s capability manually above.
            </p>
          )}
        </div>
        {message && <div className="provider-message">{message}</div>}
      </div>
    </div>
  );
}
