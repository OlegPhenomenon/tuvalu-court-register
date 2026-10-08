import { useParams, useSearchParams } from 'react-router-dom';
import { PageHeader } from '../components/PageHeader';
import { Tabs } from '../components/Tabs';

// Tab contract (ARCHITECTURE.md §8): the workspace sub-view lives in ?tab= so
// links can point straight at a tab, e.g. /cases/42?tab=hearings.
const TABS = [
  { key: 'summary', label: 'Summary' },
  { key: 'participants', label: 'Participants' },
  { key: 'documents', label: 'Documents' },
  { key: 'hearings', label: 'Hearings' },
  { key: 'decisions', label: 'Decisions' },
  { key: 'dispatch', label: 'Dispatch' },
  { key: 'tasks', label: 'Tasks' },
  { key: 'history', label: 'History' },
];

export default function CaseWorkspace() {
  const { id } = useParams();
  const [params, setParams] = useSearchParams();
  const active = params.get('tab') ?? 'summary';
  return (
    <>
      <PageHeader title={`Case ${id}`} />
      <Tabs tabs={TABS} active={active} onChange={(key) => setParams({ tab: key })} />
      <p className="muted">Case workspace — summary, participants, documents, hearings, decisions, dispatch, tasks and history.</p>
    </>
  );
}
