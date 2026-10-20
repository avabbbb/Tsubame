// User-facing error text. Backend commands reject with plain strings (and jobs
// carry error_code / error_message); this maps the known ones to a short,
// actionable message and keeps the raw text for a "Details" disclosure.

export interface UserError {
  /** Short, actionable sentence for the UI. */
  message: string;
  /** The raw backend text, shown only behind "Details". */
  detail: string;
}

/** Raw text of anything thrown by invoke, a job, or the DOM. */
export function rawErrorText(err: unknown): string {
  if (err == null) return "";
  if (typeof err === "string") return err;
  if (err instanceof Error) return err.message || String(err);
  if (typeof err === "object") {
    const record = err as Record<string, unknown>;
    const code = typeof record.error_code === "string" ? record.error_code : record.code;
    const message =
      typeof record.error_message === "string" ? record.error_message : record.message;
    if (typeof message === "string" && message) {
      return typeof code === "string" && code && !message.includes(code)
        ? code + ": " + message
        : message;
    }
    try {
      return JSON.stringify(err);
    } catch {
      return String(err);
    }
  }
  return String(err);
}

type Rule = { test: RegExp; message: string };

// Order matters: the first match wins.
const RULES: Rule[] = [
  {
    test: /database is locked|database is busy|SQLITE_BUSY/i,
    message: "The library is busy with another operation. Wait a moment and try again.",
  },
  {
    test: /\b401\b|unauthori[sz]ed|invalid[_ ]api[_ ]key|incorrect api key/i,
    message: "The provider rejected the API key. Check the key in Settings → Providers.",
  },
  {
    test: /\b403\b|forbidden|permission denied by provider/i,
    message:
      "The provider refused this request. Check that your account has access to this model.",
  },
  {
    test: /\b429\b|rate[_ ]limit|too many requests|quota/i,
    message: "The provider is rate limiting requests. Wait a minute, then retry.",
  },
  {
    test: /\b5\d\d\b|bad gateway|service unavailable|internal server error|gateway timeout/i,
    message: "The provider had a server error. Try again in a few minutes.",
  },
  {
    test: /hash mismatch|checksum|integrity|sha-?256/i,
    message:
      "A downloaded file failed verification and was discarded. Retry the install; if it repeats, check your connection.",
  },
  {
    test: /insufficient disk|no space left|not enough space|disk full|os error 112|os error 28\b/i,
    message: "Not enough disk space. Free some space, then retry.",
  },
  {
    test: /unsupported[_ ]codec|unsupported format|decode|decoder|not a wav|wav-only|riff|MEDIA_ERR_DECODE|MEDIA_ERR_SRC_NOT_SUPPORTED/i,
    message:
      "This file's audio format isn't supported for this step yet. Convert it to WAV or another common format and try again.",
  },
  {
    test: /timed? ?out|timeout|connection (refused|reset)|dns|network|failed to connect|error sending request|MEDIA_ERR_NETWORK/i,
    message: "Couldn't reach the provider. Check your network connection and retry.",
  },
  {
    test: /keyring|credential|secret service|keychain|no matching entry/i,
    message:
      "The system credential store couldn't be read. Re-enter the API key in Settings → Providers.",
  },
  {
    test: /cancel+ed/i,
    message: "The operation was cancelled.",
  },
];

/** Maps a backend error to a short actionable message plus the raw detail. */
export function formatUserError(err: unknown): UserError {
  const detail = rawErrorText(err).trim();
  if (!detail) return { message: "Something went wrong.", detail: "" };
  for (const rule of RULES) {
    if (rule.test.test(detail)) return { message: rule.message, detail };
  }
  // Unknown: a short first line is already readable; long text goes behind Details.
  const firstLine = detail.split(/\r?\n/, 1)[0];
  const message =
    firstLine.length <= 140 ? capitalize(firstLine) : "Something went wrong. See details.";
  return { message, detail };
}

function capitalize(text: string) {
  return text ? text[0].toUpperCase() + text.slice(1) : text;
}

/** Friendly one-line message for UI state that holds a string; raw text goes to the console. */
export function userMessage(error: unknown): string {
  const { message, detail } = formatUserError(error);
  if (detail && detail !== message) console.warn(detail);
  return message;
}
