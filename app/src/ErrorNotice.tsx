import { formatUserError } from "./errors";

/**
 * A friendly error line with the raw backend text behind a "Details"
 * disclosure. role="alert" so assistive tech announces it.
 */
export default function ErrorNotice({
  error,
  className = "",
}: {
  error: unknown;
  className?: string;
}) {
  const { message, detail } = formatUserError(error);
  const showDetail = detail && detail !== message;
  return (
    <div className={("error-notice " + className).trim()} role="alert">
      <span>{message}</span>
      {showDetail && (
        <details>
          <summary>Details</summary>
          <code>{detail}</code>
        </details>
      )}
    </div>
  );
}
