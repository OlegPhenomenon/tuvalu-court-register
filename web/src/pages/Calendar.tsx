import { PageHeader } from '../components/PageHeader';

export default function Calendar() {
  return (
    <>
      <PageHeader title="Calendar" />
      <p className="muted">Hearings by day, week and month with judge and room filters; scheduling, adjournment and time conflicts.</p>
    </>
  );
}
