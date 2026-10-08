import { useState } from 'react';
import { Link } from 'react-router-dom';
import { api } from '../api';
import { useSession } from '../session';
import { fmtLocal } from '../time';
import { PageHeader } from '../components/PageHeader';
import { Button } from '../components/Button';
import { Card } from '../components/Card';
import { DataTable } from '../components/DataTable';
import { ErrorBanner } from '../components/ErrorBanner';
import { DateField, SelectField, TextField } from '../components/fields';
import { staffOptions, useRef as useRefData } from '../components/refdata';
import { useApi } from '../components/useApi';
import { CasePicker } from './intake/pickers';
import type { CaseHit } from './intake/pickers';
import './admin.css';

type Event = { id: number; at: string; user_id: number | null; user_name: string | null; action: string; summary: string; case_id: number | null; entity_type: string; entity_id: number | null; ip: string | null; details?: unknown };
type Verify = { events: number; intact: boolean; first_broken_id: number | null };
function AuditBrowser() {
  const ref = useRefData();
  const [caseHit, setCaseHit] = useState<CaseHit | null>(null);
  const [user, setUser] = useState('');
  const [action, setAction] = useState('');
  const [from, setFrom] = useState('');
  const [to, setTo] = useState('');
  const [query, setQuery] = useState('');
  const events = useApi<{ events: Event[] }>(`/audit?${query}`);
  const [verification, setVerification] = useState<Verify | null>(null);
  const [verifyError, setVerifyError] = useState<unknown>(null);
  const [busy, setBusy] = useState(false);
  const verify = async () => { setBusy(true); setVerifyError(null); setVerification(null); try { setVerification(await api<Verify>('GET', '/audit/verify')); } catch (e) { setVerifyError(e); } finally { setBusy(false); } };
  return <>
    <p>The audit chain is tamper-evident, not tamper-proof: verification detects a broken hash chain; it does not prevent a server operator from changing stored data.</p>
    <Card title="Integrity" actions={<Button busy={busy} onClick={() => void verify()}>Verify integrity</Button>}>
      <ErrorBanner error={verifyError} onRetry={() => void verify()} />
      {verification && <p role="status">{verification.intact ? `Chain intact: ${verification.events} events` : `Chain broken at #${verification.first_broken_id}`}</p>}
    </Card>
    <form className="admin-filters" onSubmit={e => { e.preventDefault(); const q = new URLSearchParams(); if (caseHit) q.set('case_id', String(caseHit.id)); if (user) q.set('user_id', user); if (action.trim()) q.set('action', action.trim()); if (from) q.set('from', from); if (to) q.set('to', to); setQuery(q.toString()); events.reload(); }}>
      <CasePicker label="Case number" value={caseHit} onChange={setCaseHit} />
      <SelectField label="User" value={user} onChange={setUser} placeholder="All users" options={staffOptions(ref.data?.staff)} />
      <TextField label="Action starts with" value={action} onChange={setAction} placeholder="e.g. case." />
      <DateField label="From" value={from} onChange={setFrom} max={to || undefined} />
      <DateField label="To" value={to} onChange={setTo} min={from || undefined} />
      <Button type="submit">Apply filters</Button>
    </form>
    <ErrorBanner error={ref.error} onRetry={ref.reload} />
    <ErrorBanner error={events.error} onRetry={events.reload} />
    <p className="muted">Dates use court time (UTC+12). Up to 500 recent matching events are shown; narrow the filters to find older events.</p>
    {events.loading ? <p role="status">Loading events…</p> : !events.error && events.data && <DataTable rows={events.data.events} rowKey={r => String(r.id)} empty="No matching events." columns={[
      { key: 'id', header: 'Event' }, { key: 'at', header: 'Court time', render: r => fmtLocal(r.at) }, { key: 'user_name', header: 'User', render: r => r.user_name ?? 'System' }, { key: 'action', header: 'Action' },
      { key: 'summary', header: 'Summary', render: r => <>{r.summary}{r.case_id && <div><Link to={`/cases/${r.case_id}`}>Open case #{r.case_id}</Link></div>}</> },
      { key: 'details', header: 'Details', render: r => <details><summary>View details</summary><p>{r.entity_type} #{r.entity_id ?? '—'} · IP: {r.ip ?? '—'}</p>{r.details === undefined ? <p>Details withheld by the access policy.</p> : <pre className="admin-json">{JSON.stringify(r.details, null, 2)}</pre>}</details> },
    ]} />}
  </>;
}
export default function Audit() { const { session, hasPerm } = useSession(); return <><PageHeader title="Audit" />{hasPerm('audit.view') ? <AuditBrowser key={session.user.id} /> : <p>You do not have permission to view the audit log.</p>}</>; }
