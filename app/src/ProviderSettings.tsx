import { useCallback, useEffect, useMemo, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import type {
  ProviderConfig,
  ProviderInput,
  ProviderModel,
  ProviderPreset,
  ProviderTestResult,
} from "./types";

const EMPTY_DRAFT: ProviderInput = {
  id: null,
  name: "",
  kind: "openai-compatible",
  execution: "remote_api",
  base_url: "",
  model_list_url: "",
  auth_mode: "bearer",
  api_key: null,
  enabled: true,
};

export default function ProviderSettings() {
  const [providers, setProviders] = useState<ProviderConfig[]>([]);
  const [presets, setPresets] = useState<ProviderPreset[]>([]);
  const [capabilities, setCapabilities] = useState<string[]>([]);
  const [models, setModels] = useState<ProviderModel[]>([]);
  const [selectedId, setSelectedId] = useState<string | null>(null);
  const [draft, setDraft] = useState<ProviderInput>(EMPTY_DRAFT);
  const [apiKey, setApiKey] = useState("");
  const [modelFilter, setModelFilter] = useState("");
  const [expandedModel, setExpandedModel] = useState<string | null>(null);
  const [testResult, setTestResult] = useState<ProviderTestResult | null>(null);
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);

  const selectedProvider = useMemo(
    () => providers.find((provider) => provider.id === selectedId) ?? null,
    [providers, selectedId],
  );

  const visibleModels = useMemo(() => {
    const query = modelFilter.trim().toLowerCase();
    if (!query) return models;
    return models.filter((model) =>
      [model.model_id, model.display_name, model.owned_by]
        .join(" ")
        .toLowerCase()
        .includes(query),
    );
  }, [modelFilter, models]);

  const loadProviders = useCallback(async () => {
    const next = await invoke<ProviderConfig[]>("list_providers");
    setProviders(next);
    return next;
  }, []);

  const loadModels = useCallback(async (providerId: string) => {
    const next = await invoke<ProviderModel[]>("list_provider_models", {
      providerId,
    });
    setModels(next);
    return next;
  }, []);

  useEffect(() => {
    void (async () => {
      const [providerRows, presetRows, capabilityRows] = await Promise.all([
        invoke<ProviderConfig[]>("list_providers"),
        invoke<ProviderPreset[]>("provider_presets"),
        invoke<string[]>("provider_capabilities"),
      ]);
      setProviders(providerRows);
      setPresets(presetRows);
      setCapabilities(capabilityRows);
      if (providerRows[0]) setSelectedId(providerRows[0].id);
    })();
  }, []);

  useEffect(() => {
    if (!selectedProvider) {
      setModels([]);
      setApiKey("");
      return;
    }

    setDraft({
      id: selectedProvider.id,
      name: selectedProvider.name,
      kind: selectedProvider.kind,
      execution: selectedProvider.execution,
      base_url: selectedProvider.base_url,
      model_list_url: selectedProvider.model_list_url,
      auth_mode: selectedProvider.auth_mode,
      api_key: null,
      enabled: selectedProvider.enabled,
    });
    setApiKey("");
    setTestResult(null);
    setMessage("");
    void loadModels(selectedProvider.id);
  }, [loadModels, selectedId, selectedProvider]);

  function updateDraft<K extends keyof ProviderInput>(
    key: K,
    value: ProviderInput[K],
  ) {
    setDraft((current) => ({ ...current, [key]: value }));
  }

  const startPreset = (preset: ProviderPreset) => {
    setSelectedId(null);
    setModels([]);
    setApiKey("");
    setTestResult(null);
    setMessage("");
    setDraft({
      id: null,
      name: preset.name,
      kind: preset.kind,
      execution: preset.execution,
      base_url: preset.base_url,
      model_list_url: preset.model_list_url,
      auth_mode: preset.auth_mode,
      api_key: null,
      enabled: true,
    });
  };

  const saveProvider = async () => {
    setBusy(true);
    setMessage("");
    try {
      const input: ProviderInput = {
        ...draft,
        api_key: apiKey.length ? apiKey : null,
      };
      const saved = await invoke<ProviderConfig>("save_provider", { input });
      const next = await loadProviders();
      setSelectedId(saved.id);
      setApiKey("");
      setMessage(
        saved.secret_ref || saved.auth_mode === "none"
          ? "Provider saved."
          : "Provider saved. Add an API key before testing.",
      );
      const latest = next.find((provider) => provider.id === saved.id);
      if (latest) {
        setDraft({
          id: latest.id,
          name: latest.name,
          kind: latest.kind,
          base_url: latest.base_url,
          model_list_url: latest.model_list_url,
          auth_mode: latest.auth_mode,
          api_key: null,
          enabled: latest.enabled,
        });
      }
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  };

  const testProvider = async () => {
    if (!selectedId) {
      setMessage("Save the provider before testing.");
      return;
    }
    setBusy(true);
    setMessage("");
    try {
      const result = await invoke<ProviderTestResult>(
        "test_provider_connection",
        { id: selectedId },
      );
      setTestResult(result);
    } catch (error) {
      setTestResult({
        ok: false,
        status: "error",
        model_count: 0,
        message: String(error),
      });
    } finally {
      setBusy(false);
    }
  };

  const refreshModels = async () => {
    if (!selectedId) {
      setMessage("Save the provider before discovering models.");
      return;
    }
    setBusy(true);
    setMessage("");
    try {
      const discovered = await invoke<ProviderModel[]>(
        "refresh_provider_models",
        { id: selectedId },
      );
      setModels(discovered);
      await loadProviders();
      setMessage(`Discovered ${discovered.filter((model) => model.available).length} available models.`);
    } catch (error) {
      setMessage(String(error));
      await loadProviders();
    } finally {
      setBusy(false);
    }
  };

  const removeProvider = async () => {
    if (!selectedId) return;
    setBusy(true);
    try {
      await invoke("delete_provider", { id: selectedId });
      const next = await loadProviders();
      const nextId = next[0]?.id ?? null;
      setSelectedId(nextId);
      setModels([]);
      if (!nextId) setDraft(EMPTY_DRAFT);
      setMessage("Provider removed. Its credential reference was deleted too.");
    } catch (error) {
      setMessage(String(error));
    } finally {
      setBusy(false);
    }
  };

  const saveModelCapabilities = async (
    model: ProviderModel,
    next: string[] | null,
  ) => {
    try {
      const updated = await invoke<ProviderModel>("set_model_capabilities", {
        providerId: model.provider_id,
        modelId: model.model_id,
        capabilities: next,
      });
      setModels((rows) =>
        rows.map((row) =>
          row.model_id === updated.model_id ? updated : row,
        ),
      );
    } catch (error) {
      setMessage(String(error));
    }
  };

  const toggleCapability = (model: ProviderModel, capability: string) => {
    const current =
      model.manual_capabilities ?? model.effective_capabilities;
    const next = current.includes(capability)
      ? current.filter((value) => value !== capability)
      : [...current, capability];
    void saveModelCapabilities(model, next);
  };

  return (
    <section className="provider-settings">
      <header className="provider-settings-head">
        <div>
          <p className="eyebrow">MODEL & PROVIDER REGISTRY</p>
          <h2>Bring your own models.</h2>
          <p>
            Credentials stay outside SQLite. Tsubame discovers models, stores only
            redacted references, and resolves features by capability.
          </p>
        </div>
        <select
          className="provider-preset-select"
          value=""
          onChange={(event) => {
            const preset = presets.find((row) => row.id === event.target.value);
            if (preset) startPreset(preset);
          }}
        >
          <option value="">＋ Add provider…</option>
          {presets.map((preset) => (
            <option key={preset.id} value={preset.id}>
              {preset.name}
            </option>
          ))}
        </select>
      </header>

      <div className="provider-layout">
        <aside className="provider-list">
          <p className="section-label">PROVIDERS</p>
          {providers.map((provider) => (
            <button
              key={provider.id}
              className={`provider-list-row ${selectedId === provider.id ? "active" : ""}`}
              onClick={() => setSelectedId(provider.id)}
            >
              <span className="provider-status-dot" data-enabled={provider.enabled} />
              <span>
                <strong>{provider.name}</strong>
                <small>
                  {provider.system_managed
                    ? `${provider.execution.replace("_", " ")} · ${provider.availability}`
                    : provider.secret_ref
                      ? `${provider.execution.replace("_", " ")} · Key secured`
                      : provider.auth_mode === "none"
                        ? `${provider.execution.replace("_", " ")} · No auth`
                        : `${provider.execution.replace("_", " ")} · Key missing`}
                </small>
              </span>
            </button>
          ))}
          {!providers.length && (
            <div className="provider-list-empty">
              Add OpenAI, Alibaba Model Studio, or any compatible endpoint.
            </div>
          )}
        </aside>

        <div className="provider-editor">
          <div className="provider-form-card">
            <div className="provider-form-title">
              <div>
                <p className="section-label">CONNECTION</p>
                <h3>{draft.id ? draft.name : "New provider"}</h3>
              </div>
              {draft.id && (
                <span className="provider-id-chip">{draft.kind}</span>
              )}
            </div>

            <div className="provider-form-grid">
              <label>
                Name
                <input
                  value={draft.name}
                  disabled={selectedProvider?.system_managed}
                  onChange={(event) => updateDraft("name", event.target.value)}
                  placeholder="My provider"
                />
              </label>
              <label>
                Execution
                <select
                  value={draft.execution}
                  disabled={selectedProvider?.system_managed}
                  onChange={(event) =>
                    updateDraft(
                      "execution",
                      event.target.value as ProviderInput["execution"],
                    )
                  }
                >
                  <option value="remote_api">Remote API</option>
                  <option value="local_server">Local server</option>
                  {selectedProvider?.system_managed && (
                    <>
                      <option value="local_runtime">Local runtime</option>
                      <option value="native_os">Native OS</option>
                    </>
                  )}
                </select>
              </label>
              <label>
                Auth
                <select
                  disabled={selectedProvider?.system_managed}
                  value={draft.auth_mode}
                  onChange={(event) =>
                    updateDraft(
                      "auth_mode",
                      event.target.value as ProviderInput["auth_mode"],
                    )
                  }
                >
                  <option value="bearer">Bearer API key</option>
                  <option value="none">No authentication</option>
                </select>
              </label>
              <label className="provider-span-two">
                Inference base URL
                <input
                  value={draft.base_url}
                  disabled={selectedProvider?.system_managed}
                  onChange={(event) => updateDraft("base_url", event.target.value)}
                  placeholder="https://api.example.com/v1"
                  spellCheck={false}
                />
              </label>
              <label className="provider-span-two">
                Model discovery URL
                <input
                  value={draft.model_list_url}
                  disabled={selectedProvider?.system_managed}
                  onChange={(event) =>
                    updateDraft("model_list_url", event.target.value)
                  }
                  placeholder="Leave empty to use {base_url}/models"
                  spellCheck={false}
                />
              </label>
              {draft.auth_mode === "bearer" && !selectedProvider?.system_managed && (
                <label className="provider-span-two">
                  API key
                  <input
                    type="password"
                    autoComplete="new-password"
                    value={apiKey}
                    onChange={(event) => setApiKey(event.target.value)}
                    placeholder={
                      selectedProvider?.secret_ref
                        ? "Stored securely · leave blank to keep it"
                        : "Paste once · never stored in SQLite"
                    }
                  />
                </label>
              )}
            </div>

            <div className="provider-form-actions">
              <label className="provider-toggle">
                <input
                  type="checkbox"
                  checked={draft.enabled}
                  disabled={selectedProvider?.system_managed}
                  onChange={(event) =>
                    updateDraft("enabled", event.target.checked)
                  }
                />
                Enabled
              </label>
              <div>
                {draft.id && !selectedProvider?.system_managed && (
                  <button
                    className="secondary"
                    disabled={busy}
                    onClick={() => void removeProvider()}
                  >
                    Delete
                  </button>
                )}
                {!selectedProvider?.system_managed && (
                  <button
                    className="primary"
                    disabled={busy}
                    onClick={() => void saveProvider()}
                  >
                    {busy ? "Working…" : "Save provider"}
                  </button>
                )}
              </div>
            </div>
          </div>

          {draft.id && (
            <div className="provider-health-card">
              <div>
                <p className="section-label">CONNECTION & DISCOVERY</p>
                <div className="provider-health-copy">
                  <strong>
                    {selectedProvider?.last_error
                      ? "Needs attention"
                      : selectedProvider?.last_refresh_at
                        ? "Ready"
                        : "Not tested yet"}
                  </strong>
                  <small>
                    {selectedProvider?.system_managed
                      ? selectedProvider.message
                      : selectedProvider?.last_error ||
                        (selectedProvider?.last_refresh_at
                          ? `Last refresh · ${selectedProvider.last_refresh_at}`
                          : "Test the credential, then discover visible models.")}
                  </small>
                </div>
              </div>
              <div className="provider-health-actions">
                {selectedProvider?.system_managed ? (
                  <span className="provider-id-chip">
                    {selectedProvider.availability}
                  </span>
                ) : (
                  <>
                    <button
                      className="secondary"
                      disabled={busy}
                      onClick={() => void testProvider()}
                    >
                      Test
                    </button>
                    <button
                      className="secondary"
                      disabled={busy}
                      onClick={() => void refreshModels()}
                    >
                      Refresh models
                    </button>
                  </>
                )}
              </div>
              {testResult && (
                <div
                  className={`provider-test-result ${testResult.ok ? "ok" : "error"}`}
                >
                  <strong>{testResult.ok ? "Connected" : "Connection failed"}</strong>
                  <span>{testResult.message}</span>
                </div>
              )}
            </div>
          )}

          {draft.id && (
            <div className="provider-model-card">
              <div className="provider-model-head">
                <div>
                  <p className="section-label">MODELS</p>
                  <strong>
                    {models.filter((model) => model.available).length} available
                  </strong>
                </div>
                <input
                  value={modelFilter}
                  onChange={(event) => setModelFilter(event.target.value)}
                  placeholder="Filter models"
                />
              </div>

              <div className="provider-model-list">
                {visibleModels.map((model) => (
                  <div
                    key={model.model_id}
                    className={`provider-model-row ${!model.available ? "unavailable" : ""}`}
                  >
                    <button
                      className="provider-model-summary"
                      onClick={() =>
                        setExpandedModel((current) =>
                          current === model.model_id ? null : model.model_id,
                        )
                      }
                    >
                      <span>
                        <strong>{model.display_name || model.model_id}</strong>
                        <small>{model.model_id}</small>
                      </span>
                      <span className="provider-capability-preview">
                        {model.effective_capabilities.length
                          ? model.effective_capabilities
                              .slice(0, 3)
                              .map((capability) => (
                                <em key={capability}>{capability}</em>
                              ))
                          : <em>capability unknown</em>}
                      </span>
                      <span className="provider-capability-source">
                        {model.capability_source}
                      </span>
                    </button>

                    {expandedModel === model.model_id && (
                      <div className="provider-capability-editor">
                        <p>
                          Runtime features query these capabilities; they never branch
                          on this model ID.
                        </p>
                        <div className="provider-capability-grid">
                          {capabilities.map((capability) => (
                            <label key={capability}>
                              <input
                                type="checkbox"
                                checked={model.effective_capabilities.includes(
                                  capability,
                                )}
                                onChange={() =>
                                  toggleCapability(model, capability)
                                }
                              />
                              {capability}
                            </label>
                          ))}
                        </div>
                        <button
                          className="provider-auto-button"
                          disabled={model.manual_capabilities === null}
                          onClick={() => void saveModelCapabilities(model, null)}
                        >
                          Reset to discovered/catalog capabilities
                        </button>
                      </div>
                    )}
                  </div>
                ))}

                {!models.length && (
                  <div className="provider-model-empty">
                    No model inventory yet. Save, test, then refresh this provider.
                  </div>
                )}
              </div>
            </div>
          )}

          {message && <div className="provider-message">{message}</div>}
        </div>
      </div>
    </section>
  );
}
