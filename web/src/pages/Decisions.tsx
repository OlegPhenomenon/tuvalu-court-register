import { useState } from 'react';
import { Link } from 'react-router-dom';
import { Card } from '../components/Card';
import { DataTable } from '../components/DataTable';
import { ErrorBanner } from '../components/ErrorBanner';
import { PageHeader } from '../components/PageHeader';
import { StatusBadge } from '../components/StatusBadge';
import { SelectField } from '../components/fields';
import { useApi } from '../components/useApi';
import { DecisionChain, DecisionFile, IssuedCopies, finalisedNote } from './case/DecisionsTab';
import type { Decision } from './case/DecisionsTab';
import { fmtDate, fmtLocal } from '../time';
import { useSession } from '../session';
import './documents.css';

export default function Decisions() {
  const { session } = useSession();
  return <DecisionsList key={session.user.id} />;
}

function DecisionsList() {
  const [status, setStatus] = useState('');
  const list = useApi<{ items: Decision[] }>(`/decisions${status ? `?status=${encodeURIComponent(status)}` : ''}`);
  return <div className="doc-record">
    <PageHeader title="Decisions" />
    <p>{finalisedNote}</p>
    <Card>
      <SelectField label="Status" value={status} onChange={setStatus} placeholder="All statuses" options={[
        { value: 'draft', label: 'Draft' }, { value: 'finalised', label: 'Finalised' }, { value: 'superseded', label: 'Superseded' }, { value: 'withdrawn', label: 'Withdrawn' },
      ]} />
      <ErrorBanner error={list.error} onRetry={list.reload} />
      {list.loading ? <p role="status">Loading decisions…</p> : <DataTable rows={list.error ? [] : list.data?.items ?? []} rowKey={(d) => String(d.id)} empty={list.error ? 'Decisions could not be loaded.' : 'No accessible decisions match this status.'} columns={[
        { key: 'title', header: 'Decision', render: (d) => <><Link to={`/cases/${d.case_id}?tab=decisions#decision-${d.id}`}>{d.title}</Link><DecisionChain decision={d} decisions={list.data?.items} />{d.status_reason && <p>{d.status_reason}</p>}</> },
        { key: 'case_id', header: 'Case', render: (d) => <Link className="nowrap" to={`/cases/${d.case_id}?tab=decisions`}>{d.case_number}</Link> },
        { key: 'status', header: 'Status', render: (d) => <StatusBadge status={d.status} /> },
        { key: 'decision_date', header: 'Decision date', render: (d) => d.decision_date ? fmtDate(d.decision_date) : 'Not recorded' },
        { key: 'document_version_id', header: 'Bound version', render: (d) => <DecisionFile decision={d} /> },
        { key: 'author_name', header: 'Author' },
        { key: 'finalised_at', header: 'Finalised', render: (d) => d.finalised_at ? <>{d.finalised_by_name}<div>{fmtLocal(d.finalised_at)}</div></> : '—' },
        { key: 'issued', header: 'Issued copies', render: (d) => ['finalised', 'superseded'].includes(d.status) ? <IssuedCopies decision={d} /> : '—' },
      ]} />}
      <p className="muted">Up to 500 results. Open a case to draft, finalise or amend a decision.</p>
    </Card>
  </div>;
}
