import { useState } from "react";
import ErrorNotice from "./ErrorNotice";
import {
  cancelJob,
  canCancel,
  canRetry,
  jobProgress,
  retryJob,
  useJobs,
  type Job,
} from "./jobs";
import type { MediaItem } from "./types";

const KIND_LABELS: Record<string, string> = {
  translate: "Translate",
  tts: "Synthesize speech",
  mix: "Render dub mix",
  regenerate: "Regenerate sentence",
  runtime_install: "Install runtime",
  spatial: "Spatial render",
};

export const jobKindLabel = (kind: string) =>
  KIND_LABELS[kind] ?? kind.replace(/[_.-]+/g, " ").replace(/^\w/, (c) => c.toUpperCase());

function formatWhen(value: string) {
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : date.toLocaleString();
}

/** One job row: status, progress, error, cancel / retry. */
export function JobRow({ job, mediaTitle }: { job: Job; mediaTitle?: string }) {
  const [actionError, setActionError] = useState<unknown>(null);
  const progress = jobProgress(job);

  async function act(action: (id: string) => Promise<Job>) {
    setActionError(null);
    try {
      await action(job.id);
    } catch (err) {
      setActionError(err);
    }
  }

  return (
    <li className="job-row" data-status={job.status}>
      <div className="job-row-head">
        <strong>{jobKindLabel(job.kind)}</strong>
        <span className="job-status" data-status={job.status}>
          {job.status}
        </span>
      </div>
      <small>
        {mediaTitle ? mediaTitle + " · " : ""}
        {job.stage || (job.status === "queued" ? "waiting" : "")}
        {job.attempt > 1 ? " · attempt " + job.attempt : ""}
        {" · " + formatWhen(job.created_at)}
      </small>
      {(job.status === "running" || job.status === "queued") && (
        <div
          className="job-progress"
          role="progressbar"
          aria-label={jobKindLabel(job.kind) + " progress"}
          aria-valuemin={0}
          aria-valuemax={100}
          aria-valuenow={progress === null ? undefined : Math.round(progress * 100)}
        >
          <span
            style={{ width: progress === null ? "100%" : Math.round(progress * 100) + "%" }}
            data-indeterminate={progress === null || undefined}
          />
        </div>
      )}
      {job.status === "failed" && (job.error_message || job.error_code) && (
        <ErrorNotice error={job} />
      )}
      {actionError != null && <ErrorNotice error={actionError} />}
      {(canCancel(job) || canRetry(job)) && (
        <div className="job-actions">
          {canCancel(job) && (
            <button className="secondary" onClick={() => void act(cancelJob)}>
              Cancel
            </button>
          )}
          {canRetry(job) && (
            <button className="secondary" onClick={() => void act(retryJob)}>
              Retry
            </button>
          )}
        </div>
      )}
    </li>
  );
}

/** The Jobs rail: every job, newest first, live from job://update. */
export default function JobsView({ media }: { media: MediaItem[] }) {
  const { jobs, error, refresh } = useJobs();
  const titles = new Map(media.map((item) => [item.id, item.title]));
  const active = jobs.filter((job) => job.status === "queued" || job.status === "running");
  const finished = jobs.filter((job) => job.status !== "queued" && job.status !== "running");

  return (
    <section className="content jobs-view" aria-labelledby="jobs-heading">
      <div className="jobs-view-head">
        <div>
          <span className="section-label">JOBS</span>
          <h2 id="jobs-heading">Jobs</h2>
        </div>
        <button className="secondary" onClick={() => void refresh()}>
          Refresh
        </button>
      </div>
      {error && <ErrorNotice error={error} />}
      {jobs.length === 0 && !error ? (
        <div className="empty-state" role="status">
          <strong>No jobs yet</strong>
          <small>Translation, speech synthesis, dub renders and installs show up here.</small>
        </div>
      ) : (
        <>
          {active.length > 0 && (
            <>
              <h3 className="jobs-group">Active · {active.length}</h3>
              <ul className="job-list">
                {active.map((job) => (
                  <JobRow key={job.id} job={job} mediaTitle={titles.get(job.media_id)} />
                ))}
              </ul>
            </>
          )}
          {finished.length > 0 && (
            <>
              <h3 className="jobs-group">Finished · {finished.length}</h3>
              <ul className="job-list">
                {finished.map((job) => (
                  <JobRow key={job.id} job={job} mediaTitle={titles.get(job.media_id)} />
                ))}
              </ul>
            </>
          )}
        </>
      )}
    </section>
  );
}
