import { ApiError } from '../api';
import { Button } from './Button';

function cell(v: unknown): string {
  if (v === null || v === undefined) return '—';
  if (typeof v === 'object') return JSON.stringify(v);
  return String(v);
}

/**
 * Form/action error: `<ErrorBanner error={err} onRetry={save} attempted={draft} />`.
 * - ApiError              → server message (+ code).
 * - `version_conflict`    → "Someone else changed this record" plus a field-by-field
 *                           comparison of `attempted` vs `details.current` when given.
 * - network failure       → "Not saved — your text is kept." (typed state stays in
 *                           React state; retry re-sends it).
 */
export function ErrorBanner({ error, onRetry, attempted }: {
  error: unknown;
  onRetry?: () => void;
  /** What the user tried to save — compared against `details.current` on conflicts. */
  attempted?: Record<string, unknown>;
}) {
  if (!error) return null;

  if (error instanceof ApiError && error.code === 'version_conflict') {
    const current = (error.details as { current?: Record<string, unknown> } | null)?.current;
    const keys = attempted && current
      ? [...new Set([...Object.keys(attempted), ...Object.keys(current)])]
      : [];
    const diffs = keys.filter((k) => cell(attempted?.[k]) !== cell(current?.[k]));
    return (
      <div className="banner banner--error" role="alert">
        <p><strong>Someone else changed this record.</strong></p>
        <p>Your changes were not saved. Review the current values, then try again.</p>
        {diffs.length > 0 && (
          <div className="table-wrap">
            <table className="conflict-table">
              <thead>
                <tr><th scope="col">Field</th><th scope="col">You entered</th><th scope="col">Currently saved</th></tr>
              </thead>
              <tbody>
                {diffs.map((k) => (
                  <tr key={k}>
                    <td>{k.replace(/_/g, ' ')}</td>
                    <td>{cell(attempted?.[k])}</td>
                    <td>{cell(current?.[k])}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        )}
        {onRetry && <div className="banner-actions"><Button variant="secondary" onClick={onRetry}>Retry</Button></div>}
      </div>
    );
  }

  if (!(error instanceof ApiError) || error.status === 0) {
    return (
      <div className="banner banner--error" role="alert">
        <p><strong>Not saved — your text is kept.</strong> The server could not be reached.</p>
        {onRetry && <div className="banner-actions"><Button variant="secondary" onClick={onRetry}>Retry</Button></div>}
      </div>
    );
  }

  return (
    <div className="banner banner--error" role="alert">
      <p>
        {error.message}
        <span className="banner-code"> ({error.code})</span>
      </p>
      {onRetry && <div className="banner-actions"><Button variant="secondary" onClick={onRetry}>Retry</Button></div>}
    </div>
  );
}
