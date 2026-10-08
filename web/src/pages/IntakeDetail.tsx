import { useParams } from 'react-router-dom';
import { PageHeader } from '../components/PageHeader';

export default function IntakeDetail() {
  const { id } = useParams();
  return (
    <>
      <PageHeader title={`Incoming package ${id}`} />
      <p className="muted">Triage this intake: request information, mark ready, link to an existing case or register a new one.</p>
    </>
  );
}
