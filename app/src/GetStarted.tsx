import { useState } from "react";

/** Checklist steps, in the order a first run walks them. */
export type GetStartedStep =
  | "import"
  | "asr"
  | "transcript"
  | "edit"
  | "translationProvider"
  | "translation"
  | "ttsProvider"
  | "regenerate"
  | "dubbed";

/** Completion per step, derived by the caller from real app state. */
export type GetStartedProgress = Record<GetStartedStep, boolean>;

const DISMISS_KEY = "yuzuki.getStarted.dismissed";

const STEPS: { id: GetStartedStep; label: string; hint: string }[] = [
  { id: "import", label: "Import a local file", hint: "Add an audio or video file to the Library." },
  { id: "asr", label: "Get speech recognition ready", hint: "Install the runtime pack or add your own API key." },
  { id: "transcript", label: "Generate subtitles", hint: "Transcribe the track in Studio." },
  { id: "edit", label: "Edit one sentence", hint: "Fix the text or timing of a segment." },
  { id: "translationProvider", label: "Configure a translation provider", hint: "Add a provider in Settings." },
  { id: "translation", label: "Translate the track", hint: "Run Translate from the Inspector." },
  { id: "ttsProvider", label: "Configure a speech provider", hint: "Add a text-to-speech provider in Settings." },
  { id: "regenerate", label: "Regenerate a sentence", hint: "Synthesize a new take for one segment." },
  { id: "dubbed", label: "Play the dubbed version", hint: "Switch the player to Dubbed." },
];

/** First-run checklist. Each row deep-links through onOpen; dismissal persists. */
export default function GetStarted({
  progress,
  onOpen,
}: {
  progress: GetStartedProgress;
  onOpen: (step: GetStartedStep) => void;
}) {
  const [dismissed, setDismissed] = useState(() => {
    try {
      return localStorage.getItem(DISMISS_KEY) === "1";
    } catch {
      return false;
    }
  });
  if (dismissed) return null;

  const done = STEPS.filter((step) => progress[step.id]).length;
  const next = STEPS.find((step) => !progress[step.id]);

  function dismiss() {
    try {
      localStorage.setItem(DISMISS_KEY, "1");
    } catch {
      // Storage unavailable: hide for this session only.
    }
    setDismissed(true);
  }

  return (
    <section className="get-started" aria-labelledby="get-started-heading">
      <header className="get-started-head">
        <div>
          <p className="eyebrow">GET STARTED</p>
          <h2 id="get-started-heading">From a local file to a dubbed track</h2>
          <small>
            {done} of {STEPS.length} done
          </small>
        </div>
        <button
          type="button"
          className="secondary"
          onClick={dismiss}
          aria-label="Dismiss the Get Started checklist"
        >
          Dismiss
        </button>
      </header>
      <div
        className="get-started-progress"
        role="progressbar"
        aria-label="Get Started progress"
        aria-valuemin={0}
        aria-valuemax={STEPS.length}
        aria-valuenow={done}
      >
        <span style={{ width: (done / STEPS.length) * 100 + "%" }} />
      </div>
      <ol className="get-started-list">
        {STEPS.map((step) => {
          const complete = progress[step.id];
          return (
            <li
              key={step.id}
              data-done={complete || undefined}
              data-next={step === next || undefined}
            >
              <button
                type="button"
                onClick={() => onOpen(step.id)}
                aria-label={(complete ? "Done: " : "To do: ") + step.label + ". " + step.hint}
              >
                <span className="get-started-check" aria-hidden="true">
                  {complete ? "\u2713" : ""}
                </span>
                <span className="get-started-text">
                  <strong>{step.label}</strong>
                  <small>{step.hint}</small>
                </span>
              </button>
            </li>
          );
        })}
      </ol>
    </section>
  );
}
