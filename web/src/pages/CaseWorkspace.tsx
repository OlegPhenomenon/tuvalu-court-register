/**
 * Case workspace: header (number, title, category, status, restricted marker,
 * registration date, responsible, judge), the server-computed NextActions box,
 * and tabs routed via ?tab= so links can point straight at a tab
 * (e.g. /cases/42?tab=hearings).
 */

import type { ComponentType } from 'react';
import { useParams, useSearchParams } from 'react-router-dom';
import { RegistryPage } from './case/RegistryPage';
import { ErrorBanner } from '../components/ErrorBanner';
import { NextActions } from '../components/NextActions';
import { PageHeader } from '../components/PageHeader';
import { StatusBadge } from '../components/StatusBadge';
import { Tabs } from '../components/Tabs';
import { useApi } from '../components/useApi';
import { fmtDate } from '../time';
import SummaryTab from './case/SummaryTab';
import ParticipantsTab from './case/ParticipantsTab';
import DocumentsTab from './case/DocumentsTab';
import HearingsTab from './case/HearingsTab';
import DecisionsTab from './case/DecisionsTab';
import DispatchTab from './case/DispatchTab';
import TasksTab from './case/TasksTab';
import HistoryTab from './case/HistoryTab';
import type { CaseData, CaseTabProps } from './case/types';

const TABS: { key: string; label: string; Component: ComponentType<CaseTabProps> }[] = [
  { key: 'summary', label: 'Summary', Component: SummaryTab },
  { key: 'participants', label: 'Participants', Component: ParticipantsTab },
  { key: 'documents', label: 'Documents', Component: DocumentsTab },
  { key: 'hearings', label: 'Hearings', Component: HearingsTab },
  { key: 'decisions', label: 'Decisions', Component: DecisionsTab },
  { key: 'dispatch', label: 'Dispatch', Component: DispatchTab },
  { key: 'tasks', label: 'Tasks', Component: TasksTab },
  { key: 'history', label: 'History', Component: HistoryTab },
];

export default function CaseWorkspace() {
  const { id } = useParams();
  const [params, setParams] = useSearchParams();
  const { data, error, loading, reload } = useApi<CaseData>(id ? `/cases/${id}` : null);

  const active = TABS.some((t) => t.key === params.get('tab')) ? params.get('tab')! : 'summary';

  if ((loading && !data) || (!error && data && data.case.id !== Number(id))) {
    return (
      <>
        <PageHeader title="Case" />
        <p className="muted">Loading…</p>
      </>
    );
  }
  if (error || !data) {
    return (
      <>
        <PageHeader title="Case" />
        <ErrorBanner error={error} onRetry={reload} />
      </>
    );
  }

  const c = data.case;
  const judge = data.assignments.find((a) => a.role === 'judge' && a.end_at === null);
  const caseId = c.id;
  const tab = TABS.find((t) => t.key === active) ?? TABS[0]!;
  const { Component } = tab;

  return (
    <RegistryPage>
      <PageHeader
        eyebrow="Case"
        title={
          <>
            {c.number} <StatusBadge status={c.status} />
            {Boolean(c.restricted) && <span className="badge badge--danger">Restricted</span>}
          </>
        }
        subtitle={c.title}
      />
      <dl className="case-meta">
        <div><dt>Registered</dt><dd>{fmtDate(c.registered_date)}</dd></div>
        <div><dt>Responsible officer</dt><dd>{c.responsible_name ?? '—'}</dd></div>
        <div><dt>Judge</dt><dd>{judge?.display_name ?? '—'}</dd></div>
        <div><dt>Category</dt><dd>{c.category_label}</dd></div>
      </dl>
      <NextActions items={data.next_actions} />

      <Tabs
        idPrefix={`case-${caseId}`}
        tabs={TABS.map(({ key, label }) => ({ key, label }))}
        active={active}
        onChange={(key) =>
          setParams((prev) => {
            const p = new URLSearchParams(prev);
            p.set('tab', key);
            return p;
          })
        }
      />
      {TABS.map((t) => (
        <div
          key={t.key}
          role="tabpanel"
          id={`case-${caseId}-panel-${t.key}`}
          aria-labelledby={`case-${caseId}-tab-${t.key}`}
          hidden={t.key !== active}
          tabIndex={0}
        >
          {t.key === active && <Component key={caseId} caseId={caseId} caseData={data} reload={reload} />}
        </div>
      ))}
    </RegistryPage>
  );
}
