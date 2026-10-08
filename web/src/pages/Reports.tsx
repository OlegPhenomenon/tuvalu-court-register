import { useState } from 'react';
import { Link } from 'react-router-dom';
import { downloadUrl } from '../api';
import { useSession } from '../session';
import { courtToday, fmtCourtLocal, fmtDate, fmtLocal } from '../time';
import { PageHeader } from '../components/PageHeader';
import { Card } from '../components/Card';
import { Button } from '../components/Button';
import { DataTable } from '../components/DataTable';
import { DateField } from '../components/fields';
import { ErrorBanner } from '../components/ErrorBanner';
import { useApi } from '../components/useApi';
import './admin.css';

type Metric = { key: string; label: string; count: number; extra?: string };
type Workload = { user_id: number; display_name: string; open_cases: number; open_tasks: number };
type Summary = { period: { from: string; to: string; as_of: string }; metrics: Metric[]; workload: Workload[] };
type Items = { columns: { key: string; header: string }[]; rows: Record<string, string | number | null>[] };

function ReportsContent() {
  const today = courtToday();
  const [from, setFrom] = useState(`${today.slice(0, 7)}-01`);
  const [to, setTo] = useState(today);
  const [asOf, setAsOf] = useState(today);
  const [query, setQuery] = useState(new URLSearchParams({ from, to, as_of: asOf }).toString());
  const [metric, setMetric] = useState<Metric | null>(null);
  const metricQuery = metric?.extra ? `${query}&${metric.extra}` : query;
  const summary = useApi<Summary>(`/reports/summary?${query}`);
  const items = useApi<Items>(metric ? `/reports/${metric.key}/items?${metricQuery}` : null);
  const workloadCell = (w: Workload, key: 'workload_cases' | 'workload_tasks') => {
    const count = key === 'workload_cases' ? w.open_cases : w.open_tasks;
    const label = `${key === 'workload_cases' ? 'Open cases' : 'Open tasks'} — ${w.display_name}`;
    const extra = `user=${w.user_id}`;
    return <button type="button" className="report-count" aria-pressed={metric?.key === key && metric?.extra === extra} aria-label={`${count} ${label}`} onClick={() => setMetric({ key, label, count, extra })}>{count}</button>;
  };
  return <div className="reports-page">
    <PageHeader title="Reports" actions={<Button variant="secondary" onClick={() => window.print()}>Print</Button>} />
    <p>Counts include only cases you are allowed to see. Case age is shown as information, not as a breach of any rule.</p>
    <p className="muted">“Without a next step” and staff workload describe current work. A scheduled hearing, open task, unsent dispatch or draft decision counts as a next step. Period dates apply to registration, closure and reopening events.</p>
    <form className="admin-filters" onSubmit={e => { e.preventDefault(); setMetric(null); setQuery(new URLSearchParams({ from, to, as_of: asOf }).toString()); summary.reload(); }}>
      <DateField label="Period from" value={from} onChange={setFrom} required max={to} />
      <DateField label="Period to" value={to} onChange={setTo} required min={from} />
      <DateField label="As of" value={asOf} onChange={setAsOf} required />
      <Button type="submit">Apply dates</Button>
    </form>
    <ErrorBanner error={summary.error} onRetry={summary.reload} />
    {summary.loading ? <p role="status">Loading reports…</p> : !summary.error && summary.data && <>
      <p>Period: {fmtDate(summary.data.period.from)} – {fmtDate(summary.data.period.to)}. As of {fmtDate(summary.data.period.as_of)}.</p>
      <div className="report-metrics">{summary.data.metrics.map(m => <button type="button" className="report-metric" key={m.key} aria-pressed={metric?.key === m.key} onClick={() => setMetric(m)}><strong>{m.count}</strong><span>{m.label}</span></button>)}</div>
      <Card title="Current staff workload"><p className="muted">Select a count to see the cases behind it.</p><DataTable rows={summary.data.workload} rowKey={r => String(r.user_id)} empty="No active staff." columns={[{ key: 'display_name', header: 'Staff member' }, { key: 'open_cases', header: 'Open cases', render: w => workloadCell(w, 'workload_cases') }, { key: 'open_tasks', header: 'Open tasks', render: w => workloadCell(w, 'workload_tasks') }]} /></Card>
    </>}
    {metric && <Card title={metric.label} actions={<a href={downloadUrl(`/reports/${metric.key}/csv?${metricQuery}`)}>Download CSV</a>}>
      <ErrorBanner error={items.error} onRetry={items.reload} />
      {items.loading ? <p role="status">Loading records…</p> : !items.error && items.data && <DataTable<Record<string, string | number | null>> rows={items.data.rows} empty="No matching records." columns={items.data.columns.map((c, index) => ({ key: c.key, header: c.header, render: row => {
        const raw = row[c.key];
        // The API adds human labels next to codes (category → category_label, …).
        const labelled = row[`${c.key}_label`];
        const value = typeof labelled === 'string' && labelled ? labelled
          : raw == null ? '—'
          : typeof raw === 'string' && /^\d{4}-\d{2}-\d{2}T/.test(raw)
            // `*_local` columns carry a court-local wall clock (no zone) — never
            // read them as browser-local; other T-strings are UTC instants.
            ? (c.key.endsWith('_local') || !/(Z|[+-]\d{2}:?\d{2})$/.test(raw) ? fmtCourtLocal(raw) : fmtLocal(raw))
          : typeof raw === 'string' && /^\d{4}-\d{2}-\d{2}$/.test(raw) ? fmtDate(raw)
          : String(raw);
        return index === 0 && typeof row.link === 'string' ? <Link to={row.link}>{value}</Link> : value;
      } }))} />}
    </Card>}
  </div>;
}

export default function Reports() {
  const { session, hasPerm } = useSession();
  return hasPerm('report.view') ? <ReportsContent key={session.user.id} /> : <><PageHeader title="Reports" /><p>You do not have permission to view reports.</p></>;
}
