/**
 * Dispatch page (C09/C12): the cross-case queue — prepared notices, copy
 * packages and intake information requests. Filters by status and kind;
 * "Sent – awaiting handover confirmation" keeps only sent dispatches with no
 * recorded human handover yet.
 */

import { useMemo, useState } from 'react';
import { Button } from '../components/Button';
import { Card } from '../components/Card';
import { DispatchCard } from '../components/DispatchCard';
import type { DispatchRecord } from '../components/DispatchCard';
import { ErrorBanner } from '../components/ErrorBanner';
import { SelectField } from '../components/fields';
import { PageHeader } from '../components/PageHeader';
import { useApi } from '../components/useApi';
import './dispatch.css';

const STATUS_FILTERS = [
  { value: '', label: 'Everything' },
  { value: 'draft', label: 'Needs review' },
  { value: 'queued', label: 'Queued' },
  { value: 'failed', label: 'Failed' },
  { value: 'awaiting_handover', label: 'Sent – awaiting handover confirmation' },
  { value: 'sent', label: 'Sent' },
  { value: 'cancelled', label: 'Cancelled' },
];

const KIND_FILTERS = [
  { value: '', label: 'All kinds' },
  { value: 'notice', label: 'Notices' },
  { value: 'copies', label: 'Copy packages' },
  { value: 'information_request', label: 'Information requests' },
];

export default function Dispatch() {
  const [status, setStatus] = useState('');
  const [kind, setKind] = useState('');

  const path = useMemo(() => {
    const p = new URLSearchParams();
    // "awaiting_handover" fetches sent and is narrowed below.
    if (status && status !== 'awaiting_handover') p.set('status', status);
    if (status === 'awaiting_handover') p.set('status', 'sent');
    if (kind) p.set('kind', kind);
    const s = p.toString();
    return `/dispatches${s ? `?${s}` : ''}`;
  }, [status, kind]);

  const { data, error, loading, reload } = useApi<{ items: DispatchRecord[] }>(path);

  const items = useMemo(() => {
    const all = data?.items ?? [];
    if (status !== 'awaiting_handover') return all;
    return all.filter(
      (d) => !d.confirmations.some((c) => c.kind === 'human_handover'),
    );
  }, [data, status]);

  return (
    <>
      <PageHeader title="Dispatch" />
      <p className="muted">
        Prepared notices, copy packages and information requests across all cases you may see —
        sending, delivery confirmations and retries.
      </p>
      <Card>
        <div className="dispatch-filters">
          <SelectField label="Status" value={status} onChange={setStatus} options={STATUS_FILTERS} />
          <SelectField label="Kind" value={kind} onChange={setKind} options={KIND_FILTERS} />
          <div className="actions">
            <Button
              variant="secondary"
              onClick={() => {
                setStatus('');
                setKind('');
              }}
            >
              Clear
            </Button>
          </div>
        </div>
      </Card>
      {loading && <p className="muted">Loading…</p>}
      <ErrorBanner error={error} onRetry={reload} />
      {data && items.length === 0 && (
        <Card>
          <p className="muted">Nothing in the dispatch queue for this filter.</p>
        </Card>
      )}
      {items.map((d) => (
        <DispatchCard key={d.id} dispatch={d} showContext onChanged={reload} />
      ))}
    </>
  );
}
