/**
 * History tab (C15): the case timeline from GET /api/cases/{id}/history —
 * every audit event in order (local time, who, summary). Events touching a
 * document the user may not see arrive already redacted by the server.
 * Text filter + print.
 */

import { useMemo, useState } from 'react';
import { Button } from '../../components/Button';
import { Card } from '../../components/Card';
import { DataTable } from '../../components/DataTable';
import type { Column } from '../../components/DataTable';
import { ErrorBanner } from '../../components/ErrorBanner';
import { TextField } from '../../components/fields';
import { useApi } from '../../components/useApi';
import { fmtCourtLocal, fmtLocal } from '../../time';
import type { CaseTabProps } from './types';
import '../dispatch.css';

interface HistoryEvent {
  id: number;
  at: string;
  at_local: string;
  user_id: number | null;
  user_name: string | null;
  action: string;
  summary: string;
}

export default function HistoryTab({ caseId }: CaseTabProps) {
  const { data, error, loading, reload } = useApi<{ events: HistoryEvent[] }>(
    `/cases/${caseId}/history`,
  );
  const [filter, setFilter] = useState('');

  const events = useMemo(() => {
    const all = data?.events ?? [];
    const q = filter.trim().toLowerCase();
    if (!q) return all;
    return all.filter((ev) =>
      [ev.summary, ev.action, ev.user_name ?? '']
        .join(' ')
        .toLowerCase()
        .includes(q),
    );
  }, [data, filter]);

  const columns: Column<HistoryEvent>[] = [
    {
      key: 'at',
      header: 'When (court time)',
      render: (ev) => (ev.at_local ? fmtCourtLocal(ev.at_local) : fmtLocal(ev.at)),
    },
    {
      key: 'user_name',
      header: 'Who',
      render: (ev) => ev.user_name ?? 'system',
    },
    {
      key: 'summary',
      header: 'What happened',
      render: (ev) => (
        <>
          {ev.summary}
          <div className="muted history-action">{ev.action.replace(/\./g, ' · ')}</div>
        </>
      ),
    },
  ];

  return (
    <>
      <div className="page-actions history-toolbar">
        <TextField
          label="Filter the history"
          value={filter}
          onChange={setFilter}
          placeholder="Text in the event, person or action"
        />
        <Button variant="secondary" onClick={() => window.print()}>
          Print
        </Button>
      </div>
      <Card>
        {loading && <p className="muted">Loading…</p>}
        <ErrorBanner error={error} onRetry={reload} />
        {data && (
          <DataTable
            columns={columns}
            rows={events}
            rowKey={(ev) => String(ev.id)}
            empty={
              filter.trim()
                ? 'No history entries match the filter.'
                : 'No history entries yet.'
            }
          />
        )}
      </Card>
    </>
  );
}
