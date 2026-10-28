// Runtime/model pack install surface (docs/06-runtime-bootstrap.md).
// Mirrors runtime.rs: runtime_install_plan, install_runtime_packs, runtime_doctor.
import { useCallback, useEffect, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { ProviderModel } from "./types";
import {
  JOB_UPDATE_EVENT,
  cancelJob,
  canCancel,
  canRetry,
  jobProgress,
  retryJob,
  type Job,
} from "./jobs";
import { userMessage } from "./errors";

export type PackKind = "runtime" | "model" | "library";

export interface PlanItem {
  id: string;
  version: string;
  kind: PackKind;
  license: string;
  size: number;
  unpacked_size: number;
  required_bytes: number;
  installed: boolean;
  supported: boolean;
  active_version: string | null;
  hardware: string | null;
  sources: string[];
}

export interface InstallPlan {
  provider_id: string;
  model_id: string;
  items: PlanItem[];
  missing: string[];
  download_bytes: number;
  required_bytes: number;
  free_bytes: number | null;
  fits: boolean | null;
  ready: boolean;
  blocker: string | null;
}

export interface PackStatus {
  id: string;
  kind: PackKind;
  license: string;
  manifest_version: string;
  state: "active" | "broken" | "partial" | "missing" | "unsupported";
  active_version: string | null;
  previous_version: string | null;
  update_available: boolean;
  path: string | null;
  staged_bytes: number;
  env_override: { name: string; value: string; exists: boolean } | null;
  probe: { ok: boolean; detail: string } | null;
}

const BYTE_UNITS = ["KB", "MB", "GB"];

function formatBytes(bytes: number): string {
  if (bytes < 1024) return bytes + " B";
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < BYTE_UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return value.toFixed(value < 10 ? 1 : 0) + " " + BYTE_UNITS[unit];
}

type Props = {
  providerId: string;
  models: Pick<ProviderModel, "model_id" | "display_name">[];
  // Called after a successful install so provider availability re-resolves.
  onInstalled?: () => void;
};

export default function RuntimePacksCard({ providerId, models, onInstalled }: Props) {
  const [modelId, setModelId] = useState("");
  const [plan, setPlan] = useState<InstallPlan | null>(null);
  const [doctor, setDoctor] = useState<PackStatus[]>([]);
  const [job, setJob] = useState<Job | null>(null);
  const [error, setError] = useState("");

  useEffect(() => {
    if (!models.some((model) => model.model_id === modelId)) {
      setModelId(models[0]?.model_id ?? "");
    }
  }, [modelId, models]);

  const refresh = useCallback(async () => {
    setError("");
    try {
      const [nextPlan, nextDoctor] = await Promise.all([
        invoke<InstallPlan>("runtime_install_plan", {
          providerId,
          modelId: modelId || null,
        }),
        invoke<PackStatus[]>("runtime_doctor"),
      ]);
      setPlan(nextPlan);
      const ids = new Set(nextPlan.items.map((item) => item.id));
      setDoctor(nextDoctor.filter((status) => ids.has(status.id)));
    } catch (e) {
      setError(userMessage(e));
    }
  }, [modelId, providerId]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  const jobId = job?.id;
  useEffect(() => {
    if (!jobId) return;
    let active = true;
    const unlisten = listen<Job>(JOB_UPDATE_EVENT, (event) => {
      if (!active || event.payload.id !== jobId) return;
      setJob(event.payload);
      if (event.payload.status === "succeeded") {
        void refresh();
        onInstalled?.();
      }
    });
    return () => {
      active = false;
      void unlisten.then((stop) => stop());
    };
  }, [jobId, onInstalled, refresh]);

  const run = async (action: () => Promise<Job>) => {
    setError("");
    try {
      setJob(await action());
    } catch (e) {
      setError(userMessage(e));
    }
  };

  const running = job ? canCancel(job) : false;
  const progress = job ? jobProgress(job) : null;

  return (
    <div className="provider-health-card runtime-packs-card">
      <div>
        <div className="settings-summary-copy">
          <strong>
            {!plan
              ? "Checking runtime…"
              : plan.ready
                ? "Runtime ready"
                : "Runtime not installed"}
          </strong>
          {plan && (
            <small>
              {plan.items.filter((item) => item.installed).length} of {plan.items.length} packs installed
            </small>
          )}
        </div>
        {models.length > 1 && (
          <label className="runtime-model-select">
            Model
            <select
              value={modelId}
              disabled={running}
              onChange={(event) => setModelId(event.target.value)}
            >
              {models.map((model) => (
                <option key={model.model_id} value={model.model_id}>
                  {model.display_name || model.model_id}
                </option>
              ))}
            </select>
          </label>
        )}
        {plan && plan.items.length > 0 && (
          <details className="settings-summary-more">
            <summary>Pack details</summary>
          <ul className="runtime-pack-list">
            {plan.items.map((item) => {
              const status = doctor.find((row) => row.id === item.id);
              const state = status?.state ?? (item.installed ? "active" : "missing");
              return (
                <li key={item.id} data-state={state}>
                  <strong>{item.id}</strong>
                  <small>
                    {item.kind} · v{item.version} · {item.license} · {formatBytes(item.size)}
                    {item.hardware && (
                      <>
                        {" "}
                        · optional:{" "}
                        {item.hardware === "nvidia-cuda"
                          ? "NVIDIA GPU acceleration (CPU works without it)"
                          : item.hardware}
                      </>
                    )}
                  </small>
                  <small>
                    {state}
                    {status?.active_version && <> · active v{status.active_version}</>}
                    {status?.update_available && <> · update available</>}
                    {status && status.staged_bytes > 0 && (
                      <> · {formatBytes(status.staged_bytes)} staged (resumable)</>
                    )}
                    {status?.probe && <> · probe {status.probe.ok ? "ok" : "failed"}</>}
                  </small>
                  {status?.probe && !status.probe.ok && <small>{status.probe.detail}</small>}
                  {status?.env_override && (
                    <small>
                      Developer override {status.env_override.name} = {status.env_override.value}
                      {!status.env_override.exists && <> (path missing)</>}
                    </small>
                  )}
                </li>
              );
            })}
          </ul>
          </details>
        )}
        {plan && !plan.ready && (
          <small className="runtime-plan-summary">
            Download {formatBytes(plan.download_bytes)} · needs{" "}
            {formatBytes(plan.required_bytes)} on disk
            {plan.free_bytes !== null && <> · {formatBytes(plan.free_bytes)} free</>}
            {plan.fits === false && <> · not enough space</>}
          </small>
        )}
        {plan?.blocker && <small className="runtime-plan-blocker">{plan.blocker}</small>}
        {job && (
          <small className="runtime-job-status">
            {job.status} · {job.stage || "queued"}
            {progress !== null && <> · {progress.toLocaleString(undefined, { style: "percent" })}</>}
            {job.error_message && <> · {job.error_message}</>}
          </small>
        )}
        {error && <small className="runtime-plan-blocker">{error}</small>}
      </div>
      <div className="provider-health-actions">
        {plan && !plan.ready && !running && (
          <button
            className="primary"
            disabled={!!plan.blocker || plan.fits === false}
            onClick={() =>
              void run(() =>
                invoke<Job>("install_runtime_packs", {
                  providerId,
                  modelId: modelId || null,
                }),
              )
            }
          >
            Install
          </button>
        )}
        {job && running && (
          <button className="secondary" onClick={() => void run(() => cancelJob(job.id))}>
            Cancel
          </button>
        )}
        {job && canRetry(job) && (
          <button className="secondary" onClick={() => void run(() => retryJob(job.id))}>
            Retry
          </button>
        )}
        <button className="secondary" disabled={running} onClick={() => void refresh()}>
          Run doctor
        </button>
      </div>
    </div>
  );
}
