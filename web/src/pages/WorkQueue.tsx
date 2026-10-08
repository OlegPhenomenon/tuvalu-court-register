import { useMemo } from 'react';
import { Link } from 'react-router-dom';
import { Card } from '../components/Card';
import { ErrorBanner } from '../components/ErrorBanner';
import { PageHeader } from '../components/PageHeader';
import { useApi } from '../components/useApi';
import { fmtDate } from '../time';

/** GET /api/queue response item — server-computed next steps for this user. */
interface QueueItem {
  kind: string;
  title: string;
  message: string;
  link: string;
  case_number?: string;
  due_date?: string;
  priority?: string;
}

const KIND_LABELS: Record<string, string> = {
  intake: 'Incoming documents',
  task: 'Tasks',
  hearing: 'Hearings',
  decision: 'Decisions',
  dispatch: 'Dispatch',
  case: 'Cases',
  mailbox: 'Mailbox',
};

const kindLabel = (kind: string) => KIND_LABELS[kind] ?? kind.replace(/_/g, ' ');

export default function WorkQueue() {
  const { data, error, loading, reload } = useApi<{ items: QueueItem[] }>('/queue');

  // Group by kind, preserving the server's ordering within and across groups.
  const groups = useMemo(() => {
    const map = new Map<string, QueueItem[]>();
    for (const item of data?.items ?? []) {
      const list = map.get(item.kind) ?? [];
      list.push(item);
      map.set(item.kind, list);
    }
    return [...map.entries()];
  }, [data]);

  return (
    <>
      <PageHeader title="Work queue" />
      {loading && <p className="muted">Loading…</p>}
      <ErrorBanner error={error} onRetry={reload} />
      {!loading && !error && groups.length === 0 && (
        <Card>
          <p className="muted">Nothing needs your attention right now.</p>
        </Card>
      )}
      {groups.map(([kind, items]) => (
        <Card key={kind} title={kindLabel(kind)}>
          <ul className="queue-list">
            {items.map((item, i) => (
              <li key={`${item.link}-${i}`}>
                <Link to={item.link} className="queue-link">
                  <span className="queue-title">{item.title}</span>
                  <span className="queue-message">{item.message}</span>
                  {(item.case_number || item.due_date || item.priority) && (
                    <span className="queue-meta">
                      {item.case_number && <span className="chip">{item.case_number}</span>}
                      {item.due_date && <span>Due {fmtDate(item.due_date)}</span>}
                      {item.priority && <span className={`prio prio--${item.priority}`}>{item.priority}</span>}
                    </span>
                  )}
                </Link>
              </li>
            ))}
          </ul>
        </Card>
      ))}
    </>
  );
}
