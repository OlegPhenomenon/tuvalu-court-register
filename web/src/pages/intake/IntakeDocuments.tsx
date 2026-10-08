import { useEffect, useState } from 'react';
import { api } from '../../api';
import { Button } from '../../components/Button';
import { DataTable } from '../../components/DataTable';
import type { Column } from '../../components/DataTable';
import { DocumentLinks, DocumentUpload, VisibilityBadge } from '../../components/DocumentUpload';
import type { DocumentDetail } from '../../components/DocumentUpload';
import { ErrorBanner } from '../../components/ErrorBanner';
import { Modal } from '../../components/Modal';
import { useApi } from '../../components/useApi';
import { label, refList, useRef as useRefData } from '../../components/refdata';
import { fmtDate, fmtLocal } from '../../time';
import { useSession } from '../../session';
import '../documents.css';

export interface IntakeDocument {
  id: number; title: string; doc_type: string; visibility: string;
  document_date: string | null; received_date: string | null;
  is_paper_original: number | boolean; original_location: string | null;
  created_at: string; version_count: number;
}

export function IntakeDocuments({ intakeId, documents, canUpload, onChanged }: {
  intakeId: number; documents: IntakeDocument[]; canUpload: boolean; onChanged: () => void;
}) {
  const { session } = useSession();
  return <IntakeDocumentsContent key={session.user.id} intakeId={intakeId} documents={documents} canUpload={canUpload} onChanged={onChanged} />;
}

function IntakeDocumentsContent({ intakeId, documents, canUpload, onChanged }: {
  intakeId: number; documents: IntakeDocument[]; canUpload: boolean; onChanged: () => void;
}) {
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const intake = useApi<{ case_id: number | null; status: string }>(canUpload ? `/intakes/${intakeId}` : null);
  const linkedCase = useApi<{ participants: { party_id: number; name: string }[]; case: { status: string } }>(intake.data?.case_id ? `/cases/${intake.data.case_id}` : null);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState(false);
  const [details, setDetails] = useState<Record<number, DocumentDetail>>({});
  const [error, setError] = useState<unknown>(null);
  const [tick, setTick] = useState(0);
  // Intake detail does not include version IDs: resolve them through document detail.
  const ids = documents.map((d) => `${d.id}:${d.version_count}`).join(',');
  useEffect(() => {
    const ac = new AbortController();
    setDetails({}); setError(null);
    void Promise.all(ids ? ids.split(',').map((entry) => api<DocumentDetail>('GET', `/documents/${entry.split(':')[0]}`, undefined, { signal: ac.signal })) : [])
      .then((items) => { if (!ac.signal.aborted) setDetails(Object.fromEntries(items.map((d) => [d.id, d]))); })
      .catch((e) => { if (!ac.signal.aborted) setError(e); });
    return () => ac.abort();
  }, [ids, tick, intakeId]);
  const close = () => { if (!busy) setOpen(false); };
  const columns: Column<IntakeDocument>[] = [
    { key: 'title', header: 'Title' },
    { key: 'doc_type', header: 'Type', render: (d) => label(refList(ref, 'document_type'), d.doc_type) },
    { key: 'visibility', header: 'Visibility', render: (d) => <VisibilityBadge value={d.visibility} /> },
    { key: 'document_date', header: 'Document date', render: (d) => d.document_date ? fmtDate(d.document_date) : '—' },
    { key: 'received_date', header: 'Received', render: (d) => d.received_date ? fmtDate(d.received_date) : '—' },
    { key: 'version_count', header: 'Versions' },
    { key: 'is_paper_original', header: 'Paper original', render: (d) => d.is_paper_original ? `Received — ${d.original_location ?? 'location not recorded'}` : 'Not received' },
    { key: 'created_at', header: 'Added', render: (d) => fmtLocal(d.created_at) },
    { key: 'id', header: 'Files', render: (d) => details[d.id]?.versions.map((v) => <div key={v.id}><span>{v.filename} · v{v.version_no}</span><DocumentLinks version={v} /></div>) ?? (error ? 'Files unavailable' : 'Loading files…') },
  ];
  return <div className="doc-record">
    <ErrorBanner error={refError} onRetry={reloadRef} />
    <ErrorBanner error={error} onRetry={() => setTick((t) => t + 1)} />
    <ErrorBanner error={intake.error} onRetry={intake.reload} /><ErrorBanner error={linkedCase.error} onRetry={linkedCase.reload} />
    {canUpload && <div className="page-actions"><Button disabled={intake.loading || !!intake.error || linkedCase.loading || !!linkedCase.error || ['duplicate', 'returned_or_redirected'].includes(intake.data?.status ?? '') || linkedCase.data?.case.status === 'closed'} onClick={() => setOpen(true)}>Add document</Button></div>}
    {canUpload && linkedCase.data?.case.status === 'closed' && <p>Reopen the linked case before adding documents.</p>}
    <DataTable columns={columns} rows={documents} rowKey={(d) => String(d.id)} empty="No documents attached to this filing yet." />
    {open && <Modal title="Add intake document" open onClose={close}>
      <DocumentUpload intakeId={intakeId} participants={linkedCase.data?.participants} onUploaded={(d) => { setDetails((all) => ({ ...all, [d.id]: d })); onChanged(); }} onCancel={close} onBusyChange={setBusy} />
    </Modal>}
  </div>;
}
