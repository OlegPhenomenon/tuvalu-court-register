import { useCallback, useEffect, useState } from 'react';
import type { FormEvent } from 'react';
import { Link } from 'react-router-dom';
import { api, ApiError, downloadUrl, newKey } from '../../api';
import { useSession } from '../../session';
import { courtToday, fmtCourtLocal, fmtDate, fmtLocal } from '../../time';
import { Button } from '../../components/Button';
import { Card } from '../../components/Card';
import { ErrorBanner } from '../../components/ErrorBanner';
import { Modal } from '../../components/Modal';
import { StatusBadge } from '../../components/StatusBadge';
import { CheckboxField, DateField, SelectField, TextField } from '../../components/fields';
import { useApi } from '../../components/useApi';
import { DocumentReasonDialog, DocumentUpload, useDocumentDetails } from '../../components/DocumentUpload';
import type { DocumentDetail } from '../../components/DocumentUpload';
import type { CaseTabProps } from './types';
import '../documents.css';

export const finalisedNote = 'Finalised in this register. This is not a qualified electronic signature.';
export interface Decision {
  id: number; case_id: number; case_number: string; title: string; decision_date: string | null;
  restricted?: boolean;
  status: string; status_reason?: string | null; document_id?: number; document_title: string;
  document_version_id: number; version_no?: number; filename?: string; sha256?: string;
  hearing_id: number | null; author_name: string | null; finalised_by_name: string | null;
  finalised_at: string | null; amends_decision_id: number | null; amendment_basis: string | null;
  superseded_by_id: number | null; signed_file_uploaded: boolean; created_at: string; version: number; note?: string;
}
export function DecisionFile({ decision: d }: { decision: Decision }) {
  if (d.restricted) return <span>Restricted document</span>;
  return <a href={downloadUrl(`/document-versions/${d.document_version_id}/download`)} target="_blank" rel="noopener noreferrer">{d.filename} · v{d.version_no}<span className="doc-sr-only"> (download, new tab)</span></a>;
}
export function DecisionChain({ decision: d, decisions }: { decision: Decision; decisions?: Decision[] }) {
  const name = (id: number) => decisions?.find((item) => item.id === id)?.title ?? `decision #${id}`;
  const target = (id: number) => `/cases/${d.case_id}?tab=decisions#decision-${id}`;
  return <>
    {d.amends_decision_id && <p>Amends <Link to={target(d.amends_decision_id)}>{name(d.amends_decision_id)}</Link>{d.amendment_basis ? ` — ${d.amendment_basis}` : ''}</p>}
    {d.superseded_by_id && <p>Superseded by <Link to={target(d.superseded_by_id)}>{name(d.superseded_by_id)}</Link></p>}
  </>;
}
interface Hearing { id: number; hearing_type_label: string; starts_at: string; starts_local?: string; status: string }

function DecisionForm({ mode, decision, caseId, caseData, onClose, onSaved }: CaseTabProps & {
  mode: 'draft' | 'edit' | 'amend'; decision?: Decision; onClose: () => void; onSaved: () => void;
}) {
  const docs = useDocumentDetails(`/cases/${caseId}/documents`);
  const hearings = useApi<{ items: Hearing[] }>(mode === 'draft' ? `/cases/${caseId}/hearings` : null);
  const [title, setTitle] = useState(decision?.title ?? '');
  const [date, setDate] = useState(decision?.decision_date ?? '');
  const [versionId, setVersionId] = useState(mode === 'edit' ? String(decision?.document_version_id ?? '') : '');
  const [recordVersion, setRecordVersion] = useState(decision?.version ?? 0);
  const [hearing, setHearing] = useState('');
  const [busy, setBusy] = useState(false);
  const [uploadBusy, setUploadBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [uploadOpen, setUploadOpen] = useState(false);
  const [uploaded, setUploaded] = useState<DocumentDetail | null>(null);
  const [confirm, setConfirm] = useState(false);
  const [basis, setBasis] = useState('');
  const allDocs = [...(docs.data ?? []), ...(uploaded && !docs.data?.some((d) => d.id === uploaded.id) ? [uploaded] : [])];
  // Decision-type documents are the right thing to bind — list them first and
  // mark everything else so a party statement is not bound by mistake.
  const sortedDocs = [...allDocs].sort(
    (a, b) => Number(b.doc_type === 'decision') - Number(a.doc_type === 'decision'),
  );
  const cleanVersions = sortedDocs.flatMap((d) => d.versions.filter((v) => v.scan_status === 'clean' && (mode !== 'amend' || v.id !== decision?.document_version_id)).map((v) => ({ value: String(v.id), label: `${d.title} — ${v.filename} · v${v.version_no}${d.doc_type === 'decision' ? '' : ' (not a decision document)'}` })));
  const attempted = { title: title.trim(), decision_date: date || null, document_version_id: Number(versionId), version: recordVersion };
  const save = async (reason?: string) => {
    if (busy || !cleanVersions.some((v) => v.value === versionId)) return;
    setBusy(true); setError(null);
    if (reason) setBasis(reason);
    try {
      if (mode === 'draft') await api('POST', `/cases/${caseId}/decisions`, { title: title.trim(), decision_date: date || null, document_version_id: Number(versionId), hearing_id: hearing ? Number(hearing) : null });
      else if (mode === 'edit') await api('PATCH', `/decisions/${decision!.id}`, attempted);
      else await api('POST', `/decisions/${decision!.id}/amend`, { title: title.trim(), decision_date: date || null, document_version_id: Number(versionId), amendment_basis: reason ?? basis });
      onSaved(); onClose();
    } catch (e) { setError(e); } finally { setBusy(false); }
  };
  const submit = (e: FormEvent) => { e.preventDefault(); if (mode === 'amend') setConfirm(true); else void save(); };
  const current = error instanceof ApiError && error.code === 'version_conflict' ? (error.details as { current?: Decision })?.current : null;
  const close = useCallback(() => { if (!busy && !uploadBusy) onClose(); }, [busy, uploadBusy, onClose]);
  return <>
    <Modal title={mode === 'draft' ? 'Draft decision' : mode === 'edit' ? 'Edit draft decision' : 'Amend finalised decision'} open={!confirm} onClose={close}>
      <ErrorBanner error={docs.error} onRetry={docs.reload} /><ErrorBanner error={hearings.error} onRetry={hearings.reload} />
      <ErrorBanner error={error} attempted={attempted} />
      {current && <Button variant="secondary" onClick={() => { setRecordVersion(current.version); setError(null); }}>Use current record version after review</Button>}
      {mode === 'amend' && <p>This creates a draft amendment. Finalise it separately to supersede the original decision. Upload a separate document for the corrected text; the original file remains unchanged.</p>}
      <form onSubmit={submit}><fieldset disabled={busy || uploadBusy} className="doc-fieldset">
        <TextField label="Decision title" value={title} onChange={setTitle} required />
        <DateField label="Decision date" value={date} onChange={setDate} help={mode === 'edit' ? 'Leave blank to clear the saved decision date.' : undefined} />
        <SelectField label="Bound document version" value={versionId} onChange={setVersionId} options={cleanVersions} placeholder={docs.loading ? 'Loading document versions…' : 'Choose a clean document version'} required help="Only visible, clean versions from this case can be used. This exact version will be bound to the decision." />
        {!docs.loading && !docs.error && cleanVersions.length === 0 && <p>No clean document versions are available. A staff member with document upload permission must add the decision document.</p>}
        {mode === 'draft' && <SelectField label="Hearing (optional)" value={hearing} onChange={setHearing} placeholder="No hearing — decision without a hearing" options={(hearings.data?.items ?? []).map((h) => ({ value: String(h.id), label: `${h.hearing_type_label} — ${h.starts_local ? fmtCourtLocal(h.starts_local) : fmtLocal(h.starts_at)} (${h.status.replace(/_/g, ' ')})` }))} />}
        <div className="actions"><Button type="submit" busy={busy} disabled={docs.loading || !!docs.error || !title.trim() || !cleanVersions.some((v) => v.value === versionId)}> {mode === 'amend' ? 'Continue to amendment basis' : mode === 'edit' ? 'Save draft' : 'Create draft'}</Button>
          <Button type="button" variant="secondary" onClick={close}>Cancel</Button></div>
      </fieldset></form>
      {caseData.allowed.manage_documents && caseData.case.status !== 'closed' && <>
        <div className="actions"><Button variant="secondary" disabled={busy || uploadBusy} onClick={() => setUploadOpen((v) => !v)}>{uploadOpen ? 'Hide upload form' : 'Upload a new decision document'}</Button></div>
        {uploadOpen && <section><h3>New decision document</h3><DocumentUpload caseId={caseId} participants={caseData.participants} initialType="decision" onBusyChange={setUploadBusy} onUploaded={(d) => { setUploaded(d); const v = d.versions.at(-1); if (v?.scan_status === 'clean') setVersionId(String(v.id)); }} /></section>}
      </>}
    </Modal>
    {confirm && <DocumentReasonDialog open title={`Basis for amending ${decision?.title}`} label="Amendment basis" confirmLabel="Create draft amendment" busy={busy} error={error} onConfirm={(r) => void save(r)} onClose={() => { if (!busy) setConfirm(false); }} />}
  </>;
}

function FinaliseForm({ decision, onClose, onSaved }: { decision: Decision; onClose: () => void; onSaved: () => void }) {
  const [date, setDate] = useState(decision.decision_date ?? courtToday());
  const [signed, setSigned] = useState(decision.signed_file_uploaded);
  const [key] = useState(newKey);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const submit = async (e: FormEvent) => {
    e.preventDefault(); if (busy) return;
    setBusy(true); setError(null);
    try { await api('POST', `/decisions/${decision.id}/finalise`, { decision_date: date, signed_file_uploaded: signed }, { idempotencyKey: key }); onSaved(); onClose(); }
    catch (err) { setError(err); } finally { setBusy(false); }
  };
  return <Modal title={`Finalise ${decision.title}`} open onClose={busy ? () => {} : onClose}>
    <ErrorBanner error={error} />
    <p>Bound file: <DecisionFile decision={decision} /></p><p>{finalisedNote}</p>
    <p>After finalisation, corrections require a linked amendment.</p>
    <form onSubmit={submit}><fieldset disabled={busy} className="doc-fieldset">
      <DateField label="Decision date" value={date} onChange={setDate} required />
      <CheckboxField label="Signed scan uploaded" checked={signed} onChange={setSigned} help="Confirm only if the bound file already contains the signed scan. This checkbox does not upload or sign a file." />
      <div className="actions"><Button type="submit" busy={busy}>Finalise decision</Button><Button type="button" variant="secondary" onClick={onClose}>Cancel</Button></div>
    </fieldset></form>
  </Modal>;
}

export default function DecisionsTab(props: CaseTabProps) {
  const { session } = useSession();
  useEffect(() => { props.reload(); }, [session, props.reload]);
  return <DecisionsTabContent key={session.user.id} {...props} />;
}

function DecisionsTabContent(props: CaseTabProps) {
  const { caseId, caseData, reload } = props;
  const { session, hasPerm } = useSession();
  const decisions = useApi<{ items: Decision[] }>(`/cases/${caseId}/decisions`);
  // Needed only to resolve "Hearing — <date>" labels on decision cards.
  const hearingLinks = useApi<{ items: Hearing[] }>(
    (decisions.data?.items ?? []).some((d) => d.hearing_id != null)
      ? `/cases/${caseId}/hearings`
      : null,
  );
  const hearingById = new Map((hearingLinks.data?.items ?? []).map((h) => [h.id, h]));
  const [form, setForm] = useState<{ mode: 'draft' | 'edit' | 'amend' | 'finalise'; decision?: Decision } | null>(null);
  const closeForm = useCallback(() => setForm(null), []);
  const [withdraw, setWithdraw] = useState<Decision | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const changed = () => { decisions.reload(); reload(); };
  const judgeScope = !session.user.is_judge || caseData.assignments.some((a) => a.user_id === Number(session.user.id) && a.role === 'judge' && !a.end_at);
  const canDraft = hasPerm('decision.draft') && caseData.allowed.draft_decision && judgeScope;
  const canFinalise = hasPerm('decision.finalise') && caseData.allowed.finalise_decision && judgeScope;
  const withdrawDraft = async (reason: string) => {
    if (!withdraw || busy) return;
    setBusy(true); setError(null);
    try { await api('POST', `/decisions/${withdraw.id}/withdraw`, { reason }); setWithdraw(null); changed(); }
    catch (e) { setError(e); } finally { setBusy(false); }
  };
  const items = decisions.data?.items ?? [];
  return <div className="doc-record">
    <Card title="Decisions" actions={canDraft && <Button onClick={() => setForm({ mode: 'draft' })}>Draft decision</Button>}>
      <p>{finalisedNote}</p><ErrorBanner error={decisions.error} onRetry={decisions.reload} />
      {decisions.loading && <p role="status">Loading decisions…</p>}
      {!decisions.loading && !decisions.error && items.length === 0 && <p className="muted">No decisions on this case yet.</p>}
    </Card>
    {items.map((d) => <div id={`decision-${d.id}`} key={d.id}><Card title={<>{d.title} <StatusBadge status={d.status} /></>} actions={<>
      {d.status === 'draft' && canDraft && <><Button variant="secondary" onClick={() => setForm({ mode: 'edit', decision: d })}>Edit draft</Button><Button variant="secondary" onClick={() => { setError(null); setWithdraw(d); }}>Withdraw draft</Button></>}
      {d.status === 'draft' && canFinalise && <Button onClick={() => setForm({ mode: 'finalise', decision: d })}>Finalise</Button>}
      {d.status === 'finalised' && canFinalise && <Button variant="secondary" onClick={() => setForm({ mode: 'amend', decision: d })}>Amend finalised</Button>}
    </>}>
      <dl className="doc-meta">
        <div><dt>Decision date</dt><dd>{d.decision_date ? fmtDate(d.decision_date) : 'Not recorded'}</dd></div>
        <div><dt>Bound document version</dt><dd><DecisionFile decision={d} /><div className="muted">{d.document_title}</div>{d.sha256 && <code title={d.sha256}>{d.sha256.slice(0, 12)}…</code>}</dd></div>
        <div><dt>Author</dt><dd>{d.author_name ?? 'Not recorded'}<div className="muted">{fmtLocal(d.created_at)}</div></dd></div>
        {d.finalised_at && <div><dt>Finalised by / at</dt><dd>{d.finalised_by_name ?? 'Not recorded'}<div>{fmtLocal(d.finalised_at)}</div></dd></div>}
        {d.hearing_id && <div><dt>Hearing</dt><dd><Link to={`/cases/${caseId}?tab=hearings&hearing=${d.hearing_id}`}>{(() => { const h = hearingById.get(d.hearing_id!); return h ? `${h.hearing_type_label} — ${h.starts_local ? fmtCourtLocal(h.starts_local) : fmtLocal(h.starts_at)}` : 'View the hearing'; })()}</Link></dd></div>}
      </dl>
      <DecisionChain decision={d} decisions={items} />
      {d.status_reason && <p>Withdrawal reason: {d.status_reason}</p>}
      {['finalised', 'superseded'].includes(d.status) && <><p>Signed scan: {d.signed_file_uploaded ? 'Uploaded' : 'Not recorded'}</p><p className="muted">{finalisedNote}</p></>}
    </Card></div>)}
    {form?.mode === 'finalise' && form.decision ? <FinaliseForm decision={form.decision} onSaved={changed} onClose={closeForm} /> : form && form.mode !== 'finalise' && <DecisionForm {...props} mode={form.mode} decision={form.decision} onSaved={changed} onClose={closeForm} />}
    {withdraw && <DocumentReasonDialog open title={`Withdraw draft ${withdraw.title}`} label="Withdrawal reason" confirmLabel="Withdraw draft" danger busy={busy} error={error} onConfirm={(r) => void withdrawDraft(r)} onClose={() => { if (!busy) setWithdraw(null); }} />}
  </div>;
}
