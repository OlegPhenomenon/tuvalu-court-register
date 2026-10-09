import { useMemo } from 'react';
import { Link } from 'react-router-dom';
import { Card } from '../components/Card';
import { ErrorBanner } from '../components/ErrorBanner';
import { Icon } from '../components/icons';
import type { IconName } from '../components/icons';
import { PageHeader } from '../components/PageHeader';
import { useApi } from '../components/useApi';
import { isAdminOnly, useSession } from '../session';
import { fmtDate } from '../time';

/** GET /api/queue response item — server-computed next steps for this user. */
interface QueueItem {
  kind: string;
  title: string;
  message: string;
  link: string;
  case_number?: string | null;
  due_date?: string | null;
  /** Server sort rank: 1 = act first, 2 = normal, 3 = waiting on someone else. */
  priority?: number;
}

const KINDS: Record<string, { label: string; icon: IconName }> = {
  intake: { label: 'Incoming documents', icon: 'inbox' },
  task: { label: 'Tasks', icon: 'check' },
  hearing: { label: 'Hearings today', icon: 'calendar' },
  decision: { label: 'Decisions', icon: 'decisions' },
  dispatch: { label: 'Dispatch', icon: 'dispatch' },
  case: { label: 'Cases', icon: 'cases' },
  mailbox: { label: 'Mailbox', icon: 'mailbox' },
};

const kindOf = (kind: string) => KINDS[kind] ?? { label: kind.replace(/_/g, ' '), icon: 'queue' as IconName };

function Priority({ value }: { value?: number }) {
  if (value === 1) return <span className="badge badge--warn">Act first</span>;
  if (value === 3) return <span className="badge badge--neutral">Waiting</span>;
  return null;
}

export default function WorkQueue() {
  const { session } = useSession();
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

  const total = data?.items.length ?? 0;
  const firstName = session.user.display_name.split(' ')[0];

  return (
    <>
      <PageHeader
        title="Work queue"
        subtitle={
          data
            ? total === 0
              ? `Welcome, ${firstName}. Nothing needs your attention right now.`
              : `Welcome, ${firstName}. ${total} ${total === 1 ? 'item needs' : 'items need'} your attention.`
            : undefined
        }
      />
      {loading && <p className="muted">Loading…</p>}
      <ErrorBanner error={error} onRetry={reload} />
      {!loading && !error && groups.length === 0 && (
        <Card>
          {isAdminOnly(session.user) ? (
            <>
              <p>
                As a <strong>{session.user.title.toLowerCase()}</strong> you manage staff accounts and reference
                data. This role has no access to cases, filings, documents or judicial notes, so case lists are not
                shown to you.
              </p>
              <p className="muted">
                Open <Link to="/settings">Settings</Link> to manage users, or switch to another person (for example
                the registry clerk or the head of registry) in the menu at the top right to see the case work.
              </p>
            </>
          ) : (
            <p className="muted">Nothing needs your attention right now.</p>
          )}
        </Card>
      )}
      {groups.map(([kind, items]) => {
        const k = kindOf(kind);
        return (
          <Card key={kind} title={<>{k.label} <span className="count">{items.length}</span></>}>
            <ul className="queue-list">
              {items.map((item, i) => {
                // Case-level items already start with the case number; don't repeat it.
                const showCase = item.case_number && !item.title.startsWith(item.case_number);
                return (
                  <li key={`${item.link}-${i}`}>
                    <Link to={item.link} className="queue-link">
                      <span className={`queue-icon queue-icon--${kind}`}>
                        <Icon name={k.icon} />
                      </span>
                      <span className="queue-text">
                        <span className="queue-title">{item.title}</span>
                        <span className="queue-message">{item.message}</span>
                      </span>
                      <span className="queue-meta">
                        {showCase && <span className="chip">{item.case_number}</span>}
                        {item.due_date && <span className="chip">Due {fmtDate(item.due_date)}</span>}
                        <Priority value={item.priority} />
                      </span>
                      <Icon name="chevronRight" className="queue-chevron" />
                    </Link>
                  </li>
                );
              })}
            </ul>
          </Card>
        );
      })}
    </>
  );
}
