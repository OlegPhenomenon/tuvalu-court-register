/**
 * Documents attached to an intake (GET /api/intakes/:id → documents[]).
 * Read-only for now: the upload component (web/src/components/DocumentUpload.tsx)
 * does not exist yet. When it does, mount it below behind `canUpload` and call
 * `onChanged()` after a successful upload — no changes needed elsewhere.
 */

import { DataTable } from '../../components/DataTable';
import type { Column } from '../../components/DataTable';
import { label, refList, useRef as useRefData } from '../../components/refdata';
import { fmtDate, fmtLocal } from '../../time';

export interface IntakeDocument {
  id: number;
  title: string;
  doc_type: string;
  visibility: string;
  document_date: string | null;
  received_date: string | null;
  is_paper_original: number | boolean;
  original_location: string | null;
  created_at: string;
  version_count: number;
}

const VISIBILITY_LABELS: Record<string, string> = {
  administrative: 'Administrative',
  party_material: 'Party material',
  restricted: 'Restricted',
  judicial_note: 'Judicial note',
};

export function IntakeDocuments({
  intakeId: _intakeId,
  documents,
  canUpload: _canUpload,
  onChanged: _onChanged,
}: {
  intakeId: number;
  documents: IntakeDocument[];
  /** intake.manage holders may upload — render <DocumentUpload/> here once it exists. */
  canUpload: boolean;
  /** Call after an upload/version change so the parent reloads the intake. */
  onChanged: () => void;
}) {
  const { data: ref } = useRefData();
  const columns: Column<IntakeDocument>[] = [
    { key: 'title', header: 'Title' },
    {
      key: 'doc_type',
      header: 'Type',
      render: (d) => label(refList(ref, 'document_type'), d.doc_type),
    },
    {
      key: 'visibility',
      header: 'Visibility',
      render: (d) => VISIBILITY_LABELS[d.visibility] ?? d.visibility,
    },
    {
      key: 'document_date',
      header: 'Document date',
      render: (d) => (d.document_date ? fmtDate(d.document_date) : '—'),
    },
    {
      key: 'received_date',
      header: 'Received',
      render: (d) => (d.received_date ? fmtDate(d.received_date) : '—'),
    },
    {
      key: 'version_count',
      header: 'Versions',
    },
    {
      key: 'is_paper_original',
      header: 'Original',
      render: (d) =>
        d.is_paper_original ? `Paper — ${d.original_location ?? 'location not recorded'}` : 'Electronic',
    },
    {
      key: 'created_at',
      header: 'Added',
      render: (d) => fmtLocal(d.created_at),
    },
  ];
  return (
    <>
      <DataTable
        columns={columns}
        rows={documents}
        rowKey={(d) => String(d.id)}
        empty="No documents attached to this filing yet."
      />
      {/* DocumentUpload goes here once web/src/components/DocumentUpload.tsx exists:
          {_canUpload && <DocumentUpload intakeId={_intakeId} onUploaded={_onChanged} />} */}
    </>
  );
}
