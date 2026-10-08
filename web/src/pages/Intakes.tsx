/**
 * Incoming documents (C02): the list of filings and "Receive filing".
 * Filings are not cases yet — they are triaged on the detail page.
 */

import { useEffect, useMemo, useState } from 'react';
import { Link, useNavigate } from 'react-router-dom';
import { api, newKey } from '../api';
import { Button } from '../components/Button';
import { RegistryPage } from './case/RegistryPage';
import { Card } from '../components/Card';
import { DataTable } from '../components/DataTable';
import type { Column } from '../components/DataTable';
import { ErrorBanner } from '../components/ErrorBanner';
import { TextField } from '../components/fields';
import { Modal } from '../components/Modal';
import { PageHeader } from '../components/PageHeader';
import { StatusBadge } from '../components/StatusBadge';
import { useApi } from '../components/useApi';
import { label, refList, useRef as useRefData } from '../components/refdata';
import { useSession } from '../session';
import { fmtDate } from '../time';
import { IntakeForm, intakePayload } from './intake/IntakeForm';
import type { IntakeFormValues } from './intake/IntakeForm';

interface IntakeRow {
  id: number;
  reference: string;
  status: string;
  sender_name: string;
  channel: string;
  origin_island: string | null;
  received_date: string;
  document_date: string | null;
  entered_at: string;
  description: string;
  case_id: number | null;
  case_number: string | null;
  document_count: number;
}

const STATUS_FILTERS: { value: string; label: string }[] = [
  { value: '', label: 'All' },
  { value: 'received', label: 'Received' },
  { value: 'needs_information', label: 'Waiting for information' },
  { value: 'ready_for_registration', label: 'Ready for registration' },
  { value: 'linked_to_case', label: 'Linked' },
  { value: 'returned_or_redirected', label: 'Returned' },
  { value: 'duplicate', label: 'Duplicate' },
];

function ReceiveFilingModal({ open, onClose, onCreated }: {
  open: boolean;
  onClose: () => void;
  onCreated: (id: number, reference: string) => void;
}) {
  // One key per opened form: a retry after a network failure replays the same
  // operation on the server instead of creating a second filing.
  const [idemKey] = useState(newKey);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const submit = async (values: IntakeFormValues) => {
    setBusy(true);
    setError(null);
    try {
      const res = await api<{ id: number; reference: string }>(
        'POST',
        '/intakes',
        intakePayload(values),
        { idempotencyKey: idemKey },
      );
      onCreated(res.id, res.reference);
    } catch (e) {
      setError(e); // typed text stays in the form; Retry resends it
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal title="Receive a filing" open={open} onClose={busy ? () => {} : onClose}>
      <IntakeForm submitLabel="Receive filing" busy={busy} error={error} onSubmit={submit} onCancel={onClose} />
    </Modal>
  );
}

export default function Intakes() {
  const { hasPerm } = useSession();
  const navigate = useNavigate();
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const [status, setStatus] = useState('');
  const [q, setQ] = useState('');
  const [appliedQ, setAppliedQ] = useState('');
  const [receiveOpen, setReceiveOpen] = useState(false);

  useEffect(() => {
    const t = setTimeout(() => setAppliedQ(q), 300);
    return () => clearTimeout(t);
  }, [q]);

  const path = useMemo(() => {
    const p = new URLSearchParams();
    if (status) p.set('status', status);
    if (appliedQ.trim()) p.set('q', appliedQ.trim());
    const s = p.toString();
    return `/intakes${s ? `?${s}` : ''}`;
  }, [status, appliedQ]);

  const { data, error, loading, reload } = useApi<{ items: IntakeRow[] }>(path);

  const columns: Column<IntakeRow>[] = [
    {
      key: 'reference',
      header: 'Reference',
      render: (r) => <Link to={`/intakes/${r.id}`}>{r.reference}</Link>,
    },
    { key: 'sender_name', header: 'Sender' },
    {
      key: 'received_date',
      header: 'Received',
      render: (r) => fmtDate(r.received_date),
    },
    {
      key: 'channel',
      header: 'Channel',
      render: (r) => label(refList(ref, 'intake_channel'), r.channel),
    },
    {
      key: 'origin_island',
      header: 'Island',
      render: (r) => (r.origin_island ? label(refList(ref, 'origin_island'), r.origin_island) : '—'),
    },
    {
      key: 'document_count',
      header: 'Docs',
      render: (r) => String(r.document_count),
    },
    {
      key: 'status',
      header: 'Status',
      render: (r) => <StatusBadge status={r.status} />,
    },
    {
      key: 'case_number',
      header: 'Case',
      render: (r) =>
        r.case_id && r.case_number ? <Link to={`/cases/${r.case_id}`}>{r.case_number}</Link> : '—',
    },
  ];

  return (
    <RegistryPage>
      <PageHeader
        title="Incoming documents"
        actions={
          hasPerm('intake.manage') ? (
            <Button onClick={() => setReceiveOpen(true)}>Receive filing</Button>
          ) : undefined
        }
      />
      <ErrorBanner error={refError} onRetry={reloadRef} />
      <Card>
        <div className="page-actions" role="group" aria-label="Filter by status" style={{ marginBottom: '0.75rem' }}>
          {STATUS_FILTERS.map((f) => (
            <button
              key={f.value}
              type="button"
              className={status === f.value ? 'btn btn--primary' : 'btn btn--secondary'}
              aria-pressed={status === f.value}
              onClick={() => setStatus(f.value)}
            >
              {f.label}
            </button>
          ))}
        </div>
        <TextField
          label="Search"
          value={q}
          onChange={setQ}
          placeholder="Reference, sender or description"
        />
        {loading && <p className="muted">Loading…</p>}
        <ErrorBanner error={error} onRetry={reload} />
        {data && (
          <DataTable
            columns={columns}
            rows={data.items}
            rowKey={(r) => String(r.id)}
            empty="No filings match this filter."
          />
        )}
      </Card>
      {receiveOpen && <ReceiveFilingModal
        open={receiveOpen}
        onClose={() => setReceiveOpen(false)}
        onCreated={(id) => {
          setReceiveOpen(false);
          navigate(`/intakes/${id}`);
        }}
      />}
    </RegistryPage>
  );
}
