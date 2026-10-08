import { PageHeader } from '../components/PageHeader';

export default function Mailbox() {
  return (
    <>
      <PageHeader title="Mailbox" />
      <p className="muted">Local mailbox viewer — outgoing notices stay on this server; nothing is sent to real addresses.</p>
    </>
  );
}
