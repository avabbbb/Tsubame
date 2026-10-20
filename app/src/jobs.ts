// Jobs API (PR #10): types mirroring `app/src-tauri/src/jobs.rs`, thin command
// wrappers, and a `useJobs` hook that keeps a list current from `job://update`.
import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { userMessage } from "./errors";

export const JOB_UPDATE_EVENT = "job://update";

export type JobStatus = "queued" | "running" | "succeeded" | "failed" | "cancelled";

/** One `jobs` row; also the `job://update` event payload. */
export interface Job {
  id: string;
  kind: string;
  media_id: number;
  segment_id: number | null;
  status: JobStatus;
  progress_done: number;
  progress_total: number;
  stage: string;
  error_code: string | null;
  error_message: string | null;
  retryable: boolean;
  attempt: number;
  input_json: unknown;
  output_json: unknown | null;
  created_at: string;
  updated_at: string;
}

export interface JobFilter {
  media_id?: number;
  segment_id?: number;
  kind?: string;
  status?: JobStatus;
  limit?: number;
}

export const isTerminal = (status: JobStatus) =>
  status === "succeeded" || status === "failed" || status === "cancelled";

export const canRetry = (job: Job) => job.status === "failed" && job.retryable;

export const canCancel = (job: Job) => job.status === "queued" || job.status === "running";

/** Fraction 0..1, or null when the job has not reported a total. */
export const jobProgress = (job: Job): number | null =>
  job.progress_total > 0 ? Math.min(1, job.progress_done / job.progress_total) : null;

export const listJobs = (filter?: JobFilter) => invoke<Job[]>("list_jobs", { filter: filter ?? null });
export const cancelJob = (id: string) => invoke<Job>("cancel_job", { id });
export const retryJob = (id: string) => invoke<Job>("retry_job", { id });

function matches(job: Job, filter?: JobFilter): boolean {
  if (!filter) return true;
  if (filter.media_id !== undefined && job.media_id !== filter.media_id) return false;
  if (filter.segment_id !== undefined && job.segment_id !== filter.segment_id) return false;
  if (filter.kind !== undefined && job.kind !== filter.kind) return false;
  if (filter.status !== undefined && job.status !== filter.status) return false;
  return true;
}

/**
 * Jobs matching `filter`, newest first, kept live from `job://update`.
 * Events that arrive before the initial `list_jobs` resolves are not lost.
 */
export function useJobs(filter?: JobFilter) {
  const [jobs, setJobs] = useState<Job[]>([]);
  const [error, setError] = useState<string | null>(null);
  const key = JSON.stringify(filter ?? {});
  const filterRef = useRef(filter);
  filterRef.current = filter;

  const refresh = useCallback(async () => {
    try {
      const rows = await listJobs(filterRef.current);
      setJobs((current) => {
        // Keep any row an event delivered that is newer than the listing.
        const byId = new Map(rows.map((row) => [row.id, row]));
        for (const job of current) {
          const listed = byId.get(job.id);
          if (!listed || listed.updated_at < job.updated_at) byId.set(job.id, job);
        }
        return [...byId.values()].sort((a, b) => (a.created_at < b.created_at ? 1 : -1));
      });
      setError(null);
    } catch (err) {
      setError(userMessage(err));
    }
  }, [key]);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | undefined;
    setJobs([]);
    void listen<Job>(JOB_UPDATE_EVENT, ({ payload }) => {
      if (disposed) return;
      setJobs((current) => {
        const rest = current.filter((job) => job.id !== payload.id);
        if (!matches(payload, filterRef.current)) return rest;
        return [payload, ...rest].sort((a, b) => (a.created_at < b.created_at ? 1 : -1));
      });
    }).then((fn) => {
      if (disposed) fn();
      else unlisten = fn;
    });
    void refresh();
    return () => {
      disposed = true;
      unlisten?.();
    };
  }, [key, refresh]);

  return {
    jobs,
    error,
    refresh,
    cancel: cancelJob,
    retry: retryJob,
  };
}
