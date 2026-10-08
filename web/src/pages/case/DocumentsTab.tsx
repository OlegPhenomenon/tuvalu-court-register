import { useCallback, useEffect, useState } from 'react';
import type { FormEvent } from 'react';
import { api, ApiError } from '../../api';
import { useSession } from '../../session';
import { fmtDate, fmtLocal } from '../../time';
import { Button } from '../../components/Button';
import { Card } from '../../components/Card';
import { DataTable } from '../../components/DataTable';
import { ErrorBanner } from '../../components/ErrorBanner';
import { Modal } from '../../components/Modal';
import { CheckboxField, SelectField, TextField } from '../../components/fields';
import { staffOptions } from '../../components/refdata';
import { useApi } from '../../components/useApi';
import { DocumentUpload, DocumentVersions, DocumentReasonDialog, VisibilityBadge, useDocumentDetails, visibilityHelp, visibilityOptions } from '../../components/DocumentUpload';
import type { DocumentDetail, DocumentGrant } from '../../components/DocumentUpload';
import type { CaseTabProps } from './types';
import '../documents.css';

interface GrantDocument { id: number; title: string; grants?: DocumentGrant[] }

function EditDocument({ document: d, canRestrict, onClose, onSaved }: {
  document: DocumentDetail; canRestrict: boolean; onClose: () => void; onSaved: () => void;
}) {
  const [title, setTitle] = useState(d.title);
  const [visibility, setVisibility] = useState(d.visibility);
  const [location, setLocation] = useState(d.original_location ?? '');
  const [hold, setHold] = useState(!!d.legal_hold);
  const [version, setVersion] = useState(d.version);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const attempted = { title: title.trim(), visibility, original_location: location.trim(), legal_hold: hold, version };
  const submit = async (e: FormEvent) => {
    e.preventDefault(); setBusy(true); setError(null);
    try { await api('PATCH', `/documents/${d.id}`, attempted); onSaved(); onClose(); }
    catch (err) { setError(err); } finally { setBusy(false); }
  };
  const current = error instanceof ApiError && error.code === 'version_conflict' ? (error.details as { current?: { version: number } })?.current : null;
  return <Modal title={`Edit ${d.title}`} open onClose={busy ? () => {} : onClose}>
    <ErrorBanner error={error} attempted={attempted} />
    {current && <Button variant="secondary" onClick={() => { setVersion(current.version); setError(null); }}>Use current record version after review</Button>}
    <form onSubmit={submit}><fieldset disabled={busy} className="doc-fieldset">
      <TextField label="Title" value={title} onChange={setTitle} required />
      <SelectField label="Visibility" value={visibility} onChange={setVisibility} disabled={d.visibility === 'judicial_note'} options={visibilityOptions.filter((v) => d.visibility === 'judicial_note' ? v.value === 'judicial_note' : v.value !== 'judicial_note' && (canRestrict || (d.visibility === 'restricted' ? v.value === 'restricted' : v.value !== 'restricted')))} help={visibilityHelp[visibility]} />
      <TextField label="Paper original location" value={location} onChange={setLocation} required={!!d.is_paper_original} help="A scan is not the paper original" />
      <CheckboxField label="Legal hold" checked={hold} onChange={setHold} help="Keep these materials for proceedings; do not destroy them." />
      <div className="actions"><Button type="submit" busy={busy} disabled={!title.trim()}>Save changes</Button><Button type="button" variant="secondary" onClick={onClose}>Cancel</Button></div>
    </fieldset></form>
  </Modal>;
}

function GrantManagement({ document, judicial, onChanged }: { document: GrantDocument; judicial?: boolean; onChanged: () => void }) {
  const [open, setOpen] = useState(false);
  const candidates = useApi<{ items: { id: number; display_name: string; title: string | null; is_judge: number }[] }>(open ? `/documents/${document.id}/grants/candidates` : null);
  const [user, setUser] = useState('');
  const [action, setAction] = useState<'grant' | DocumentGrant | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const run = async (reason: string) => {
    if (!action || busy) return;
    setBusy(true); setError(null);
    try {
      if (action === 'grant') await api('POST', `/documents/${document.id}/grants`, { user_id: Number(user), reason });
      else await api('DELETE', `/documents/${document.id}/grants/${action.id}`, { reason });
      setAction(null); setOpen(false); setUser(''); onChanged();
    } catch (e) { setError(e); } finally { setBusy(false); }
  };
  return <>
    <ErrorBanner error={candidates.error} onRetry={candidates.reload} />
    <Button variant="secondary" onClick={() => { setOpen(true); setError(null); }}>{judicial ? 'Share with…' : 'Grant access'}</Button>
    <DataTable rows={document.grants ?? []} rowKey={(g) => String(g.id)} empty="No explicit access grants." columns={[
      { key: 'user_name', header: 'Person' }, { key: 'reason', header: 'Reason' },
      { key: 'granted_at', header: 'Granted', render: (g) => <>{fmtLocal(g.granted_at)}<div className="muted">{g.granted_by_name}</div></> },
      { key: 'revoked_at', header: 'Access', render: (g) => g.revoked_at ? `Revoked ${fmtLocal(g.revoked_at)}` : 'Active' },
      { key: 'id', header: 'Action', render: (g) => !g.revoked_at && <Button variant="secondary" onClick={() => { setError(null); setAction(g); }}>Revoke</Button> },
    ]} />
    {open && !action && <Modal title={judicial ? `Share ${document.title}` : `Grant access to ${document.title}`} open onClose={() => setOpen(false)}>
      <p>Choose staff who already have access to this case. The server checks their current case access before sharing.</p>
      <SelectField label="Staff member" value={user} onChange={setUser} options={staffOptions(candidates.data?.items)} placeholder="Choose a staff member" required />
      <div className="actions"><Button disabled={!user || candidates.loading || !!candidates.error} onClick={() => setAction('grant')}>Continue</Button><Button variant="secondary" onClick={() => setOpen(false)}>Cancel</Button></div>
    </Modal>}
    {action && <DocumentReasonDialog open title={action === 'grant' ? `${judicial ? 'Share' : 'Grant access to'} ${document.title}` : `Revoke access for ${action.user_name}`} label="Reason" confirmLabel={action === 'grant' ? 'Grant access' : 'Revoke access'} danger={action !== 'grant'} busy={busy} error={error} onConfirm={(r) => void run(r)} onClose={() => { if (!busy) { setAction(null); setError(null); } }} />}
  </>;
}

export default function DocumentsTab(props: CaseTabProps) {
  const { session } = useSession();
  useEffect(() => { props.reload(); }, [session, props.reload]);
  return <DocumentsTabContent key={session.user.id} {...props} />;
}

function DocumentsTabContent({ caseId, caseData, reload }: CaseTabProps) {
  const { session, hasPerm } = useSession();
  const canGrant = hasPerm('document.grant_restricted') && caseData.allowed.grant_restricted;
  const docs = useDocumentDetails(`/cases/${caseId}/documents`);
  const restricted = useApi<{ items: GrantDocument[] }>(canGrant ? `/cases/${caseId}/restricted-documents` : null);
  const [modal, setModal] = useState<{ mode: 'add' | 'version' | 'edit'; document?: DocumentDetail } | null>(null);
  const [uploadBusy, setUploadBusy] = useState(false);
  const changed = () => { docs.reload(); restricted.reload(); reload(); };
  const close = useCallback(() => { if (!uploadBusy) setModal(null); }, [uploadBusy]);
  const canUpload = hasPerm('document.manage') && caseData.allowed.manage_documents && caseData.case.status !== 'closed';
  return <div className="doc-record">
    <Card title="Documents" actions={canUpload && <Button onClick={() => setModal({ mode: 'add' })}>Add document</Button>}>
      <p>Access to a case does not include restricted documents.</p>
      {caseData.case.status === 'closed' && <p className="muted">Reopen the case before uploading a document or a new version.</p>}
      <ErrorBanner error={docs.error} onRetry={docs.reload} />
      {docs.loading && <p role="status">Loading documents and versions…</p>}
      {docs.data?.length === 0 && <p className="muted">No visible documents on this case yet.</p>}
    </Card>
    {visibilityOptions.map((group) => {
      const items = docs.data?.filter((d) => d.visibility === group.value) ?? [];
      return items.length > 0 && <section key={group.value} aria-label={group.label}>
        <h2><VisibilityBadge value={group.value} /></h2><p className="muted">{visibilityHelp[group.value]}</p>
        {items.map((d) => {
          const author = d.created_by === Number(session.user.id);
          const canEdit = hasPerm('document.manage') && caseData.allowed.manage_documents && (d.visibility !== 'judicial_note' || author);
          const frozen = d.used_by.decisions.some((v) => ['finalised', 'superseded'].includes(v.status));
          return <Card key={d.id} title={d.title} actions={canEdit && <>
            <Button variant="secondary" onClick={() => setModal({ mode: 'edit', document: d })}>Edit</Button>
            {canUpload && <Button variant="secondary" disabled={frozen} onClick={() => setModal({ mode: 'version', document: d })}>New version</Button>}
          </>}>
            <VisibilityBadge value={d.visibility} />
            <dl className="doc-meta">
              <div><dt>Type and source</dt><dd>{d.doc_type_label} · {d.source}{d.source_party_name ? ` — ${d.source_party_name}` : ''}</dd></div>
              <div><dt>Document date / Received date</dt><dd>{d.document_date ? fmtDate(d.document_date) : 'Not recorded'} / {d.received_date ? fmtDate(d.received_date) : 'Not recorded'}</dd></div>
              <div><dt>Paper original</dt><dd>{d.is_paper_original ? `Received — ${d.original_location ?? 'location not recorded'}` : 'Not received'}{d.legal_hold ? ' · Legal hold' : ''}</dd></div>
            </dl>
            {frozen && <p>A finalised decision uses this document. New versions are refused; upload a separate document and record a decision amendment.</p>}
            <DocumentVersions versions={d.versions} />
            {d.visibility === 'judicial_note' && author && <><h3>Explicit sharing</h3><GrantManagement document={d} judicial onChanged={changed} /></>}
          </Card>;
        })}
      </section>;
    })}
    {canGrant && <Card title="Restricted documents — access management">
      <p>Access to a case does not include restricted documents. These titles and grants are available for access management; a grant is required to open a file. You cannot grant yourself access.</p>
      <ErrorBanner error={restricted.error} onRetry={restricted.reload} />
      {restricted.loading && <p role="status">Loading access records…</p>}
      {restricted.data?.items.length === 0 && <p className="muted">No restricted documents.</p>}
      {restricted.data?.items.map((d) => <section key={d.id}><h3>{d.title}</h3><GrantManagement document={d} onChanged={changed} /></section>)}
    </Card>}
    {modal?.mode === 'edit' && modal.document && <EditDocument document={modal.document} canRestrict={canGrant || modal.document.created_by === Number(session.user.id)} onSaved={changed} onClose={close} />}
    {modal && modal.mode !== 'edit' && <Modal title={modal.mode === 'version' ? `New version — ${modal.document?.title}` : 'Add document'} open onClose={close}>
      <DocumentUpload caseId={caseId} document={modal.document} participants={caseData.participants} onUploaded={changed} onCancel={close} onBusyChange={setUploadBusy} />
    </Modal>}
  </div>;
}
