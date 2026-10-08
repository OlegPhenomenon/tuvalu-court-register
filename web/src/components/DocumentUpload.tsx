import { useEffect, useId, useState } from 'react';
import type { FormEvent } from 'react';
import { api, ApiError, downloadUrl, newKey, upload } from '../api';
import { useSession } from '../session';
import { courtToday, fmtLocal } from '../time';
import { Button } from './Button';
import { ConfirmReasonDialog } from './ConfirmReasonDialog';
import { DataTable } from './DataTable';
import { ErrorBanner } from './ErrorBanner';
import { CheckboxField, DateField, SelectField, TextArea, TextField } from './fields';
import { options, refList, useRef as useRefData } from './refdata';
import '../pages/documents.css';

export interface DocumentVersion {
  id: number; version_no: number; filename: string; content_type: string;
  size_bytes: number; sha256: string; scan_status: string; scan_note: string | null;
  note: string | null; uploaded_by: number; uploaded_by_name: string | null; uploaded_at: string;
}
export interface DocumentGrant {
  id: number; user_id: number; user_name: string; reason: string;
  granted_by_name: string | null; granted_at: string; revoked_at: string | null;
}
export interface DocumentRecord {
  id: number; case_id: number | null; case_number: string | null; intake_id: number | null;
  title: string; doc_type: string; doc_type_label: string; source: string;
  source_party_id: number | null; source_party_name: string | null;
  document_date: string | null; received_date: string | null; visibility: string;
  is_paper_original: number; original_location: string | null; legal_hold: number;
  created_by: number; created_by_name: string | null; created_at: string; version: number;
  version_count: number; latest_version_id: number | null;
}
export interface DocumentDetail extends DocumentRecord {
  versions: DocumentVersion[]; grants?: DocumentGrant[];
  used_by: { decisions: { id: number; title: string; status: string }[]; dispatches: { id: number; status: string }[] };
}
export const visibilityOptions = [
  { value: 'administrative', label: 'Administrative' },
  { value: 'party_material', label: 'Party material' },
  { value: 'restricted', label: 'Restricted' },
  { value: 'judicial_note', label: 'Judicial note' },
];
export const visibilityHelp: Record<string, string> = {
  administrative: 'Administrative — staff on the case',
  party_material: 'Party material — staff on the case',
  restricted: 'Restricted — only people given access. Access to a case does not include restricted documents.',
  judicial_note: 'Judicial note — only you, judges only. You may explicitly share it with staff who have case access.',
};
export function VisibilityBadge({ value }: { value: string }) {
  return <span className={`badge badge--${value === 'restricted' ? 'warn' : value === 'judicial_note' ? 'info' : 'neutral'}`}>
    {visibilityOptions.find((v) => v.value === value)?.label ?? value}
  </span>;
}
export function DocumentLinks({ version }: { version: DocumentVersion }) {
  if (version.scan_status === 'pending_scan') return <span className="doc-quarantine">Safety check pending — cannot be opened</span>;
  if (version.scan_status !== 'clean') return <span className="doc-quarantine">Quarantined — failed the safety check, cannot be opened</span>;
  const path = `/document-versions/${version.id}/download`;
  return <span className="doc-links">
    <a href={downloadUrl(path)} target="_blank" rel="noopener noreferrer">Download<span className="doc-sr-only"> {version.filename}, version {version.version_no} (new tab)</span></a>
    {['application/pdf', 'image/jpeg', 'image/png'].includes(version.content_type) &&
      <a href={downloadUrl(`${path}?inline=1`)} target="_blank" rel="noopener noreferrer">Open<span className="doc-sr-only"> {version.filename}, version {version.version_no} (new tab)</span></a>}
  </span>;
}
export function DocumentVersions({ versions }: { versions: DocumentVersion[] }) {
  return <DataTable rows={versions} rowKey={(v) => String(v.id)} empty="No file versions." columns={[
    { key: 'version_no', header: 'Version', render: (v) => `v${v.version_no}` },
    { key: 'filename', header: 'File', render: (v) => <>{v.filename}<div className="muted">{v.size_bytes.toLocaleString('en')} bytes</div></> },
    { key: 'uploaded_at', header: 'Uploaded', render: (v) => <>{v.uploaded_by_name ?? `User #${v.uploaded_by}`}<div className="muted">{fmtLocal(v.uploaded_at)}</div></> },
    { key: 'sha256', header: 'SHA-256', render: (v) => <code title={v.sha256}>{v.sha256.slice(0, 12)}…</code> },
    { key: 'scan_status', header: 'Safety', render: (v) => <><span className={`badge badge--${v.scan_status === 'clean' ? 'success' : 'danger'}`}>{v.scan_status === 'clean' ? 'Checks passed' : v.scan_status === 'pending_scan' ? 'Check pending' : 'Quarantined'}</span>{v.scan_note && <div>{v.scan_note}</div>}</> },
    { key: 'note', header: 'Note' },
    { key: 'id', header: 'Files', render: (v) => <DocumentLinks version={v} /> },
  ]} />;
}

/** Lists contain no version metadata; load details under the same server access policy. */
export function useDocumentDetails(path: string | null) {
  const { session } = useSession();
  const [data, setData] = useState<DocumentDetail[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [tick, setTick] = useState(0);
  const [loading, setLoading] = useState(false);
  useEffect(() => {
    const ac = new AbortController();
    setData(null); setError(null); setLoading(path !== null);
    if (path) void api<{ items: DocumentRecord[] }>('GET', path, undefined, { signal: ac.signal })
      .then(async (list) => {
        const items: DocumentDetail[] = [];
        // Bound concurrent requests when cross-case search returns hundreds of documents.
        for (let start = 0; start < list.items.length; start += 8) {
          if (ac.signal.aborted) return items;
          items.push(...await Promise.all(list.items.slice(start, start + 8).map((d) => api<DocumentDetail>('GET', `/documents/${d.id}`, undefined, { signal: ac.signal }))));
        }
        return items;
      })
      .then((items) => { if (!ac.signal.aborted) setData(items); })
      .catch((e) => { if (!ac.signal.aborted) setError(e); })
      .finally(() => { if (!ac.signal.aborted) setLoading(false); });
    return () => ac.abort();
  }, [path, tick, session]);
  return { data, error, loading, reload: () => setTick((t) => t + 1) };
}

/** Keep server errors visible above the reason dialog without clearing the typed reason. */
export function DocumentReasonDialog({ error, ...props }: Parameters<typeof ConfirmReasonDialog>[0] & { error: unknown }) {
  return <><ConfirmReasonDialog {...props} />{props.open && error ? <div className="doc-reason-feedback"><ErrorBanner error={error} /></div> : null}</>;
}

export function DocumentUpload({ caseId, intakeId, document, participants = [], initialType = '', onUploaded, onCancel, onBusyChange }: {
  caseId?: number; intakeId?: number; document?: DocumentDetail;
  participants?: { party_id: number; name: string }[]; initialType?: string;
  onUploaded: (document: DocumentDetail) => void; onCancel?: () => void;
  onBusyChange?: (busy: boolean) => void;
}) {
  const { session } = useSession();
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const [key] = useState(newKey);
  const fileId = useId();
  const [file, setFile] = useState<File | null>(null);
  const [title, setTitle] = useState('');
  const [type, setType] = useState(initialType);
  const [source, setSource] = useState('court');
  const [party, setParty] = useState('');
  const [documentDate, setDocumentDate] = useState('');
  const [receivedDate, setReceivedDate] = useState(courtToday);
  const [visibility, setVisibility] = useState('administrative');
  const [paper, setPaper] = useState(false);
  const [location, setLocation] = useState('');
  const [note, setNote] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [stored, setStored] = useState<DocumentDetail | null>(null);
  const submit = async (e: FormEvent) => {
    e.preventDefault();
    if (!file || busy || stored) return;
    setBusy(true); onBusyChange?.(true); setError(null);
    const form = new FormData();
    form.set('file', file); form.set('note', note.trim());
    if (!document) {
      form.set('title', title.trim()); form.set('doc_type', type); form.set('source', source);
      form.set('source_party_id', source === 'party' ? party : '');
      form.set('document_date', documentDate); form.set('received_date', receivedDate);
      form.set('visibility', visibility); form.set('is_paper_original', String(paper));
      form.set('original_location', paper ? location.trim() : '');
    }
    try {
      const result = await upload<DocumentDetail>(document ? `/documents/${document.id}/versions` : intakeId !== undefined ? `/intakes/${intakeId}/documents` : `/cases/${caseId}/documents`, form, { idempotencyKey: key });
      setStored(result); onUploaded(result);
    } catch (err) { setError(err); }
    finally { setBusy(false); onBusyChange?.(false); }
  };
  if (stored) {
    const latest = stored.versions.reduce((a, b) => a.version_no > b.version_no ? a : b);
    return <div role="status" className="doc-upload-result">
      <p><strong>{stored.title} — version {latest.version_no} stored.</strong></p>
      <p>Stored SHA-256 checksum: <code className="doc-checksum">{latest.sha256}</code></p>
      <DocumentLinks version={latest} />
      {latest.scan_note && <p>{latest.scan_note}</p>}
      {onCancel && <div className="actions"><Button variant="secondary" onClick={onCancel}>Done</Button></div>}
    </div>;
  }
  const partyOptions = Array.from(new Map(participants.map((p) => [p.party_id, p])).values()).map((p) => ({ value: String(p.party_id), label: p.name }));
  return <form onSubmit={submit} className="doc-form">
    <ErrorBanner error={refError} onRetry={reloadRef} />
    <ErrorBanner error={error} />
    {error instanceof ApiError && error.status === 413 && <p role="alert" className="doc-quarantine">Upload refused: the file exceeds the size limit or the demo storage quota. {error.message}</p>}
    {error instanceof ApiError && error.status === 415 && <p role="alert" className="doc-quarantine">Upload refused: use PDF, DOCX, JPEG or PNG with a matching filename extension. HTML, SVG and programs are refused.</p>}
    <fieldset disabled={busy} className="doc-fieldset">
      <div className="field"><label htmlFor={fileId} className="field-label">File <span className="req">*</span></label>
        <input id={fileId} className="input" type="file" accept=".pdf,.docx,.jpg,.jpeg,.png" required aria-describedby={`${fileId}-help`} onChange={(e) => setFile(e.target.files?.[0] ?? null)} />
        <p id={`${fileId}-help`} className="field-help">PDF, DOCX, JPEG or PNG; HTML, SVG and programs are refused</p>
      </div>
      {document ? <p>New version of <strong>{document.title}</strong>. Previous versions remain available. A document used by a finalised decision cannot receive a new version; upload a separate document and record an amendment.</p> : <>
        <TextField label="Title" value={title} onChange={setTitle} required />
        <SelectField label="Document type" value={type} onChange={(v) => { setType(v); if (v === 'judicial_note') setVisibility(v); else if (type === 'judicial_note') setVisibility('administrative'); }} options={options(refList(ref, 'document_type').filter((r) => session.user.is_judge || r.code !== 'judicial_note'))} placeholder="Choose the document type" required />
        <SelectField label="Source" value={source} onChange={setSource} options={[{ value: 'court', label: 'Court' }, { value: 'party', label: 'Party' }, { value: 'external', label: 'External' }]} />
        {source === 'party' && <SelectField label="Source party" value={party} onChange={setParty} options={partyOptions} placeholder="Choose a case participant" help={partyOptions.length ? 'Case participants.' : 'No case participants are available for this filing. You may leave this unset.'} />}
        <DateField label="Document date" value={documentDate} onChange={setDocumentDate} />
        <DateField label="Received date" value={receivedDate} onChange={setReceivedDate} />
        <SelectField label="Visibility" value={visibility} onChange={(v) => { setVisibility(v); if (v === 'judicial_note') setType(v); }} options={visibilityOptions.filter((v) => session.user.is_judge || v.value !== 'judicial_note')} help={visibilityHelp[visibility]} required disabled={type === 'judicial_note'} />
        <ul className="field-help">{visibilityOptions.map((v) => <li key={v.value}>{visibilityHelp[v.value]}</li>)}</ul>
        <CheckboxField label="Paper original received" checked={paper} onChange={setPaper} help="A scan is not the paper original" />
        {paper && <TextField label="Paper original location" value={location} onChange={setLocation} required />}
      </>}
      <TextArea label="Note" value={note} onChange={setNote} required={!!document} help={document ? 'Explain why this version is being added.' : undefined} />
      <div className="actions"><Button type="submit" busy={busy} disabled={!file || (document ? !note.trim() : !title.trim() || !type || (paper && !location.trim()))}>Upload{document ? ' new version' : ' document'}</Button>
        {onCancel && <Button type="button" variant="secondary" onClick={onCancel}>Cancel</Button>}
      </div>
    </fieldset>
  </form>;
}
