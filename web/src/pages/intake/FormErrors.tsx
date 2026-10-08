import type { RefObject } from 'react';
import { ApiError } from '../../api';
import { Button } from '../../components/Button';
import { ErrorBanner } from '../../components/ErrorBanner';

/** Render outside the form: Retry submits once and runs native field validation. */
export function FormErrors({ error, form, attempted, onReviewVersion }: {
  error: unknown;
  form: RefObject<HTMLFormElement | null>;
  attempted?: Record<string, unknown>;
  onReviewVersion?: (version: number) => void;
}) {
  const conflict = error instanceof ApiError && error.code === 'version_conflict';
  const current = conflict ? (error.details as { current?: { version?: number } })?.current : null;
  return <>
    {conflict && <p role="alert">{error.message}</p>}
    <ErrorBanner error={error} attempted={attempted}
      onRetry={error && !conflict ? () => form.current?.requestSubmit() : undefined} />
    {conflict && onReviewVersion && typeof current?.version === 'number' && <p>
      <Button type="button" variant="secondary" onClick={() => onReviewVersion(current.version!)}>
        Keep my edits and use current version
      </Button>
    </p>}
  </>;
}
