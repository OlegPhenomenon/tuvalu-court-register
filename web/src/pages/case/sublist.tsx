/**
 * Minimal read-only list for the case tabs that other agents will fully
 * implement (documents, hearings, decisions, dispatch, tasks, history).
 * Renders whatever rows the endpoint returns — generically: a label, an
 * optional status badge, and a date — and shows "Not available yet" only when
 * the endpoint itself answers 404 (the route does not exist yet).
 */

import { useEffect, useState } from 'react';
import { api, ApiError } from '../../api';
import { Card } from '../../components/Card';
import { DataTable } from '../../components/DataTable';
import type { Column } from '../../components/DataTable';
import { ErrorBanner } from '../../components/ErrorBanner';
import { StatusBadge } from '../../components/StatusBadge';
import { useApi } from '../../components/useApi';
import { fmtDate, fmtLocal } from '../../time';
import type { CaseTabProps } from './types';

type Row = Record<string, unknown>;

/** Find the row array in the response: a bare array, or the first array field (items, hearings…). */
function rowsOf(data: unknown): Row[] {
  if (Array.isArray(data)) return data as Row[];
  if (data && typeof data === 'object') {
    for (const v of Object.values(data)) {
      if (Array.isArray(v)) return v as Row[];
    }
  }
  return [];
}

const LABEL_KEYS = ['title', 'subject', 'message', 'summary', 'event_type', 'action', 'number', 'hearing_type', 'kind', 'name', 'reference'];
const DETAIL_KEYS = ['recipient_name', 'description', 'summary', 'assignee_name', 'room_name'];
const DATE_KEYS = ['starts_at', 'due_date', 'decision_date', 'sent_at', 'prepared_at', 'document_date', 'received_date', 'created_at', 'at'];

function pick(row: Row, keys: string[]): string | undefined {
  for (const k of keys) {
    const v = row[k];
    if (typeof v === 'string' && v) return v;
    if (typeof v === 'number') return String(v);
  }
  return undefined;
}

function isInstant(s: string): boolean {
  return s.includes('T');
}

export function SublistCard({ title, path, empty }: { title: string; path: string; empty: string }) {
  const { data, error, loading, reload } = useApi<unknown>(path);
  const [missingRoute, setMissingRoute] = useState(false);
  const [checkingRoute, setCheckingRoute] = useState(false);

  useEffect(() => {
    setMissingRoute(false);
    setCheckingRoute(false);
    if (!(error instanceof ApiError) || error.status !== 404) return;
    let alive = true;
    setCheckingRoute(true);
    // Axum responds 405 to OPTIONS on an existing GET route, even when the
    // object is hidden or missing. Its route fallback responds 404 instead.
    // A case/object 404 must retain the server error, not imply an unfinished tab.
    api('OPTIONS', path).catch((routeError) => {
      if (alive && routeError instanceof ApiError && routeError.status === 404) setMissingRoute(true);
    }).finally(() => { if (alive) setCheckingRoute(false); });
    return () => { alive = false; };
  }, [error, path]);

  if (loading) {
    return (
      <Card title={title}>
        <p className="muted">Loading…</p>
      </Card>
    );
  }
  if (checkingRoute) return <Card title={title}><p className="muted">Loading…</p></Card>;
  if (missingRoute) {
    return (
      <Card title={title}>
        <p className="muted">Not available yet.</p>
      </Card>
    );
  }
  if (error) {
    return (
      <Card title={title}>
        <ErrorBanner error={error} onRetry={reload} />
      </Card>
    );
  }

  const rows = rowsOf(data);
  const columns: Column<Row>[] = [
    {
      key: '_label',
      header: 'Item',
      render: (r) => {
        const detail = pick(r, DETAIL_KEYS);
        return (
          <>
            <strong>{pick(r, LABEL_KEYS) ?? `#${String(r.id ?? '')}`}</strong>
            {detail && <div className="muted">{detail}</div>}
          </>
        );
      },
    },
    {
      key: '_status',
      header: 'Status',
      render: (r) => (typeof r.status === 'string' ? <StatusBadge status={r.status} /> : '—'),
    },
    {
      key: '_date',
      header: 'Date',
      render: (r) => {
        const d = pick(r, DATE_KEYS);
        return d ? (isInstant(d) ? fmtLocal(d) : fmtDate(d)) : '—';
      },
    },
  ];

  return (
    <Card title={title}>
      <DataTable
        columns={columns}
        rows={rows}
        rowKey={(r) => String(r.id ?? JSON.stringify(r))}
        empty={empty}
      />
    </Card>
  );
}

/** Convenience: a whole stub tab is just its list card. */
export function makeListTab(title: string, pathOf: (caseId: number) => string, empty: string) {
  return function ListTab({ caseId }: CaseTabProps) {
    return <SublistCard title={title} path={pathOf(caseId)} empty={empty} />;
  };
}
