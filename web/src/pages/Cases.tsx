/**
 * Cases (C03/C14): search accessible cases by number, title, party, category,
 * status, dates and staff. Cases the user may not see simply are not returned.
 */

import { useMemo, useState } from 'react';
import type { FormEvent } from 'react';
import { Link, useLocation } from 'react-router-dom';
import { Button } from '../components/Button';
import { RegistryPage } from './case/RegistryPage';
import { Card } from '../components/Card';
import { DataTable } from '../components/DataTable';
import type { Column } from '../components/DataTable';
import { ErrorBanner } from '../components/ErrorBanner';
import { DateField, SelectField, TextField } from '../components/fields';
import { PageHeader } from '../components/PageHeader';
import { StatusBadge } from '../components/StatusBadge';
import { useApi } from '../components/useApi';
import { label, options, refList, staffOptions, useRef as useRefData } from '../components/refdata';
import { fmtDate, fmtLocal } from '../time';

interface CaseRow {
  id: number;
  number: string;
  legacy_number: string | null;
  title: string;
  category: string;
  status: string;
  restricted: number;
  registered_date: string;
  closed_date: string | null;
  historical_incomplete: number;
  responsible_name: string | null;
  parties: string | null;
  judge_name: string | null;
  next_hearing_at: string | null;
  next_hearing_local?: string | null;
  has_final_decision: number;
}

const CASE_STATUSES = ['registered', 'active', 'on_hold', 'closed', 'reopened'];

interface Filters {
  q: string;
  status: string;
  category: string;
  responsible: string;
  judge: string;
  from: string;
  to: string;
}

const EMPTY_FILTERS: Filters = { q: '', status: '', category: '', responsible: '', judge: '', from: '', to: '' };

export default function Cases() {
  const location = useLocation();
  const endedAssignment = (location.state as { assignmentEnded?: { name: string; roles: string[] } } | null)?.assignmentEnded;
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const [form, setForm] = useState<Filters>(EMPTY_FILTERS);
  const [applied, setApplied] = useState<Filters>(EMPTY_FILTERS);

  const path = useMemo(() => {
    const p = new URLSearchParams();
    for (const [k, v] of Object.entries(applied)) {
      if (v.trim()) p.set(k, v.trim());
    }
    const s = p.toString();
    return `/cases${s ? `?${s}` : ''}`;
  }, [applied]);

  const { data, error, loading, reload } = useApi<{ items: CaseRow[] }>(path);

  const set = (k: keyof Filters) => (v: string) => setForm((f) => ({ ...f, [k]: v }));

  const apply = (e: FormEvent) => {
    e.preventDefault();
    setApplied(form);
  };

  const statusOptions = [
    { value: '', label: 'Any status' },
    ...CASE_STATUSES.map((s) => ({
      value: s,
      // Match StatusBadge labels.
      label: { registered: 'Registered', active: 'Active', on_hold: 'On hold', closed: 'Closed', reopened: 'Reopened' }[s] ?? s,
    })),
  ];

  const columns: Column<CaseRow>[] = [
    {
      key: 'number',
      header: 'Number',
      render: (r) => (
        <>
          <Link to={`/cases/${r.id}`}>{r.number}</Link>
          {r.legacy_number && <div className="muted">was {r.legacy_number}</div>}
        </>
      ),
    },
    { key: 'title', header: 'Title' },
    {
      key: 'parties',
      header: 'Parties',
      render: (r) => r.parties ?? '—',
    },
    {
      key: 'category',
      header: 'Category',
      render: (r) => label(refList(ref, 'case_category'), r.category),
    },
    { key: 'status', header: 'Status', render: (r) => <StatusBadge status={r.status} /> },
    { key: 'judge_name', header: 'Judge', render: (r) => r.judge_name ?? '—' },
    { key: 'responsible_name', header: 'Responsible', render: (r) => r.responsible_name ?? '—' },
    {
      key: 'next_hearing_at',
      header: 'Next hearing',
      render: (r) => (r.next_hearing_at ? fmtLocal(r.next_hearing_at) : '—'),
    },
    {
      key: 'registered_date',
      header: 'Registered',
      render: (r) => fmtDate(r.registered_date),
    },
    {
      key: 'restricted',
      header: 'Flags',
      render: (r) => (
        <>
          {Boolean(r.restricted) && <span className="badge badge--danger">Restricted</span>}{' '}
          {Boolean(r.has_final_decision) && <span className="badge badge--success">Final decision</span>}{' '}
          {Boolean(r.historical_incomplete) && <span className="badge badge--warn">Incomplete history</span>}
        </>
      ),
    },
  ];

  return (
    <RegistryPage>
      <PageHeader title="Cases" />
      {endedAssignment && <div className="next-actions" role="status">
        Assignment ended for {endedAssignment.name}. Remaining assignment roles: {endedAssignment.roles.join(', ') || 'None'}.
      </div>}
      <ErrorBanner error={refError} onRetry={reloadRef} />
      <Card>
        <form onSubmit={apply} aria-label="Search and filter cases">
          <TextField
            label="Search"
            value={form.q}
            onChange={set('q')}
            placeholder="Number, title or party name"
          />
          <SelectField label="Status" value={form.status} onChange={set('status')} options={statusOptions} />
          <SelectField
            label="Category"
            value={form.category}
            onChange={set('category')}
            options={options(refList(ref, 'case_category'))}
            placeholder="Any category"
          />
          <SelectField
            label="Responsible officer"
            value={form.responsible}
            onChange={set('responsible')}
            options={staffOptions(ref?.staff)}
            placeholder="Anyone"
          />
          <SelectField
            label="Judge"
            value={form.judge}
            onChange={set('judge')}
            options={staffOptions((ref?.staff ?? []).filter((s) => s.is_judge))}
            placeholder="Any judge"
          />
          <DateField label="Registered from" value={form.from} onChange={set('from')} />
          <DateField label="Registered to" min={form.from || undefined} value={form.to} onChange={set('to')} />
          <div className="actions">
            <Button type="submit">Search</Button>
            <Button
              type="button"
              variant="secondary"
              onClick={() => {
                setForm(EMPTY_FILTERS);
                setApplied(EMPTY_FILTERS);
              }}
            >
              Clear
            </Button>
          </div>
        </form>
      </Card>
      <Card>
        {loading && <p className="muted">Loading…</p>}
        <ErrorBanner error={error} onRetry={reload} />
        {data && (
          <DataTable
            columns={columns}
            rows={data.items}
            rowKey={(r) => String(r.id)}
            empty="No cases match — or none that you may see."
          />
        )}
      </Card>
    </RegistryPage>
  );
}
