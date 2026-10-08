/**
 * Intake detail (C02): one filing with its dates, documents, correspondence and
 * supplements, plus the triage actions the server allows for its status.
 */

import { useCallback, useEffect, useState } from 'react';
import type { ReactNode } from 'react';
import { Link, useNavigate, useParams } from 'react-router-dom';
import { api, newKey, ApiError } from '../api';
import { Button } from '../components/Button';
import { RegistryPage } from './case/RegistryPage';
import { Card } from '../components/Card';
import { ConfirmReasonDialog } from '../components/ConfirmReasonDialog';
import { DataTable } from '../components/DataTable';
import type { Column } from '../components/DataTable';
import { ErrorBanner } from '../components/ErrorBanner';
import { SelectField, TextArea, TextField } from '../components/fields';
import { Modal } from '../components/Modal';
import { PageHeader } from '../components/PageHeader';
import { NextActions } from '../components/NextActions';
import { StatusBadge } from '../components/StatusBadge';
import { useApi } from '../components/useApi';
import { label, options, refList, useRef as useRefData } from '../components/refdata';
import { fmtDate, fmtLocal } from '../time';
import { IntakeForm, intakeFormValues, intakePayload } from './intake/IntakeForm';
import type { IntakeFormValues } from './intake/IntakeForm';
import { IntakeDocuments } from './intake/IntakeDocuments';
import type { IntakeDocument } from './intake/IntakeDocuments';
import { CasePicker, IntakePicker } from './intake/pickers';
import type { CaseHit, IntakeHit } from './intake/pickers';
import { RegisterForm, registerPayload } from './intake/RegisterForm';
import type { RegisterValues } from './intake/RegisterForm';
import { useSession } from '../session';

interface IntakeRecord {
  id: number;
  reference: string;
  status: string;
  sender_party_id: number | null;
  sender_name: string;
  channel: string;
  origin_island: string | null;
  document_date: string | null;
  received_date: string;
  entered_at: string;
  description: string;
  is_paper_original: number;
  paper_location: string | null;
  missing_items: string | null;
  parent_intake_id: number | null;
  duplicate_of_intake_id: number | null;
  case_id: number | null;
  status_reason: string | null;
  created_by: number | null;
  created_by_name: string | null;
  version: number;
  updated_at: string;
}

interface IntakeMessage {
  id: number;
  direction: string;
  body: string;
  dispatch_id: number | null;
  created_at: string;
  created_by_name: string | null;
}

interface RelatedIntake {
  id: number;
  reference: string;
  status: string;
  received_date: string;
  description: string;
}

interface IntakeDispatch {
  id: number;
  kind: string;
  recipient_name: string;
  method: string;
  subject: string;
  status: string;
  prepared_at: string;
}

interface ChecksumMatch {
  sha256: string;
  document_id: number;
  title: string;
  intake_id: number | null;
  intake_reference: string | null;
  case_id: number | null;
  case_number: string | null;
}

interface IntakeDetailData {
  intake: IntakeRecord;
  case: { id: number; number: string; title: string; status: string } | null;
  documents: IntakeDocument[];
  messages: IntakeMessage[];
  related_intakes: RelatedIntake[];
  dispatches: IntakeDispatch[];
  checksum_matches: ChecksumMatch[];
  allowed_actions: string[];
  /** Server-computed "what happens next" (same shape as the case workspace). */
  next_actions?: { code: string; message: string; link?: string }[];
}

type ActionKind =
  | 'edit'
  | 'supplement'
  | 'request_info'
  | 'mark_ready'
  | 'mark_duplicate'
  | 'return'
  | 'link'
  | 'register';

/** Plain-language "what happens next" per intake status (spec §5, §7). */
function nextSteps(d: IntakeDetailData): { code: string; message: string; link?: string }[] {
  const { intake } = d;
  switch (intake.status) {
    case 'received':
      return [{
        code: 'triage',
        message: 'Check the filing: mark it ready for registration, or request missing information.',
      }];
    case 'needs_information':
      return [{
        code: 'waiting',
        message: `Waiting for: ${intake.missing_items ?? 'the requested information'}. When it arrives, add it as a supplement.`,
      }];
    case 'ready_for_registration':
      return [{
        code: 'register',
        message: 'Checked — register it as a new case, or link it to an existing case.',
      }];
    case 'linked_to_case':
      return d.case
        ? [{ code: 'linked', message: `Part of case ${d.case.number} — work continues there.`, link: `/cases/${d.case.id}` }]
        : [{ code: 'linked', message: 'Linked to a case.' }];
    case 'duplicate':
      return [{
        code: 'duplicate',
        message: `Kept as a duplicate — no new case is created.${intake.status_reason ? ` Reason: ${intake.status_reason}` : ''}`,
      }];
    case 'returned_or_redirected':
      return [{
        code: 'returned',
        message: `Returned or redirected.${intake.status_reason ? ` Recorded basis: ${intake.status_reason}` : ''}`,
      }];
    default:
      return [];
  }
}

function Detail({ label: term, children }: { label: string; children: ReactNode }) {
  return (
    <tr>
      <th scope="row">{term}</th>
      <td>{children}</td>
    </tr>
  );
}

export default function IntakeDetail() {
  const { id } = useParams();
  const navigate = useNavigate();
  const { hasPerm } = useSession();
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const { data, error, loading, reload } = useApi<IntakeDetailData>(id ? `/intakes/${id}` : null);

  const [action, setAction] = useState<ActionKind | null>(null);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<unknown>(null);
  // One idempotency key per open of a mutating form.
  const [idemKey, setIdemKey] = useState('');
  const [editVersion, setEditVersion] = useState<number | null>(null);
  const [returnReason, setReturnReason] = useState('');
  const [requestInfoSent, setRequestInfoSent] = useState(false);
  const [sentDispatchId, setSentDispatchId] = useState<number | null>(null);

  // Action-scoped form state
  const [note, setNote] = useState('');
  const [missingItems, setMissingItems] = useState('');
  const [method, setMethod] = useState('post');
  const [address, setAddress] = useState('');
  const [dupTarget, setDupTarget] = useState<IntakeHit | null>(null);
  const [dupReason, setDupReason] = useState('');
  const [linkTarget, setLinkTarget] = useState<CaseHit | null>(null);
  const [linkNote, setLinkNote] = useState('');
  const [lastAttempted, setLastAttempted] = useState<Record<string, unknown> | undefined>(undefined);

  useEffect(() => { setAction(null); setActionError(null); }, [id]);

  const open = (kind: ActionKind) => {
    setAction(kind);
    setReturnReason('');
    setEditVersion(data?.intake.version ?? null);
    setActionError(null);
    setRequestInfoSent(false);
    setSentDispatchId(null);
    setIdemKey(newKey());
    setNote('');
    setMissingItems('');
    setMethod('post');
    setAddress('');
    setDupTarget(null);
    setDupReason('');
    setLinkTarget(null);
    setLinkNote('');
    setLastAttempted(undefined);
  };
  const closeAction = useCallback(() => { if (!busy) { setAction(null); setActionError(null); } }, [busy]);

  const run = async (fn: () => Promise<void>) => {
    setBusy(true);
    setActionError(null);
    try {
      await fn();
    } catch (e) {
      setActionError(e);
    } finally {
      setBusy(false);
    }
  };

  if ((loading && !data) || (!error && data && data.intake.id !== Number(id))) return <p className="muted">Loading…</p>;
  if (error || !data) {
    return (
      <>
        <PageHeader title="Filing" />
        <ErrorBanner error={error} onRetry={reload} />
      </>
    );
  }

  const intake = data.intake;
  const allowed = new Set(data.allowed_actions);
  const steps = nextSteps(data);

  const submitEdit = (values: IntakeFormValues) =>
    run(async () => {
      const body = { ...intakePayload(values), version: editVersion ?? intake.version };
      setLastAttempted(body as Record<string, unknown>);
      await api('PATCH', `/intakes/${intake.id}`, body);
      setAction(null);
      reload();
    });

  const submitSupplement = (values: IntakeFormValues) =>
    run(async () => {
      await api('POST', `/intakes/${intake.id}/supplement`, intakePayload(values), { idempotencyKey: idemKey });
      setAction(null);
      reload();
    });

  const submitRequestInfo = () =>
    run(async () => {
      const res = await api<{ ok: boolean; dispatch_id: number }>(
        'POST',
        `/intakes/${intake.id}/request-info`,
        {
          missing_items: missingItems,
          method: method || null,
          address: address || null,
        }, { idempotencyKey: idemKey },
      );
      setSentDispatchId(res.dispatch_id ?? null);
      setRequestInfoSent(true);
      reload();
    });

  const submitMarkReady = () =>
    run(async () => {
      await api('POST', `/intakes/${intake.id}/mark-ready`, { note: note || null }, { idempotencyKey: idemKey });
      setAction(null);
      reload();
    });

  const submitDuplicate = () =>
    run(async () => {
      await api('POST', `/intakes/${intake.id}/mark-duplicate`, {
        duplicate_of_intake_id: dupTarget?.id,
        reason: dupReason,
      }, { idempotencyKey: idemKey });
      setAction(null);
      reload();
    });

  const submitReturn = async (reason: string) => {
    setReturnReason(reason);
    await run(async () => {
      try {
        await api('POST', `/intakes/${intake.id}/return`, { reason }, { idempotencyKey: idemKey });
        setAction(null);
        reload();
      } catch (error) {
        setAction(null); // The shared reason dialog has no error slot; show the error and retained reason below.
        throw error;
      }
    });
  };

  const submitLink = () =>
    run(async () => {
      await api('POST', `/intakes/${intake.id}/link`, {
        case_id: linkTarget?.id,
        note: linkNote || null,
      }, { idempotencyKey: idemKey });
      setAction(null);
      reload();
    });

  const submitRegister = (values: RegisterValues) =>
    run(async () => {
      const { body, error: msg } = registerPayload(values);
      if (msg || !body) {
        setActionError(new ApiError(400, 'validation', msg ?? 'The form is incomplete.'));
        return;
      }
      const res = await api<{ case_id: number; number: string }>(
        'POST',
        `/intakes/${intake.id}/register`,
        body,
        { idempotencyKey: idemKey },
      );
      navigate(`/cases/${res.case_id}`);
    });

  const ACTION_BUTTONS: { kind: ActionKind; label: string; danger?: boolean }[] = [
    { kind: 'edit', label: 'Edit' },
    { kind: 'supplement', label: 'Add supplement' },
    { kind: 'request_info', label: 'Request information' },
    { kind: 'mark_ready', label: 'Mark ready for registration' },
    { kind: 'mark_duplicate', label: 'Mark as duplicate' },
    { kind: 'return', label: 'Return or redirect', danger: true },
    { kind: 'link', label: 'Link to existing case' },
    { kind: 'register', label: 'Register as a new case' },
  ];

  const dupColumns: Column<RelatedIntake>[] = [
    {
      key: 'reference',
      header: 'Reference',
      render: (r) => <Link to={`/intakes/${r.id}`}>{r.reference}</Link>,
    },
    { key: 'status', header: 'Status', render: (r) => <StatusBadge status={r.status} /> },
    { key: 'received_date', header: 'Received', render: (r) => fmtDate(r.received_date) },
    { key: 'description', header: 'Description' },
  ];

  return (
    <RegistryPage>
      <PageHeader
        title={
          <>
            {intake.reference} <StatusBadge status={intake.status} />
          </>
        }
      />

      {/* Server next_actions include e.g. "Review and send the information request…"
          deep links; the local steps remain as a fallback for older responses. */}
      <NextActions items={data.next_actions ?? steps} />
      <ErrorBanner error={refError} onRetry={reloadRef} />
      {!action && Boolean(actionError) && <>
        {returnReason && <p>Recorded basis: {returnReason}</p>}
        <ErrorBanner error={actionError} onRetry={() => void submitReturn(returnReason)} />
      </>}

      {ACTION_BUTTONS.some((b) => allowed.has(b.kind)) && (
        <div className="page-actions" style={{ marginBottom: '1rem' }}>
          {ACTION_BUTTONS.filter((b) => allowed.has(b.kind)).map((b) => (
            <Button
              key={b.kind}
              variant={b.danger ? 'danger' : b.kind === 'register' ? 'primary' : 'secondary'}
              onClick={() => open(b.kind)}
            >
              {b.label}
            </Button>
          ))}
        </div>
      )}

      {data.checksum_matches.length > 0 && (
        <div className="banner" role="alert" style={{ background: '#fdeacc', border: '1px solid #ecc97e', color: '#5d3c00' }}>
          <p>
            <strong>The same file was already received.</strong> Nothing was merged — decide whether
            this is a supplement or a duplicate.
          </p>
          <ul>
            {data.checksum_matches.map((m, i) => (
              <li key={i}>
                “{m.title}” already exists
                {m.intake_reference && <> in filing {m.intake_reference}</>}
                {m.case_number && (
                  <>
                    {' '}in case <Link to={`/cases/${m.case_id}`}>{m.case_number}</Link>
                  </>
                )}
              </li>
            ))}
          </ul>
        </div>
      )}

      <Card title="Details">
        <div className="table-wrap">
          <table>
            <tbody>
              <Detail label="Reference">{intake.reference}</Detail>
              <Detail label="Sender">{intake.sender_name}</Detail>
              <Detail label="Channel">{label(refList(ref, 'intake_channel'), intake.channel)}</Detail>
              <Detail label="Island of origin">
                {intake.origin_island ? label(refList(ref, 'origin_island'), intake.origin_island) : '—'}{' '}
                <span className="muted">(the island of origin does not decide jurisdiction)</span>
              </Detail>
              {/* spec §4: the three dates are recorded and shown separately */}
              <Detail label="Document date">{intake.document_date ? fmtDate(intake.document_date) : '—'}</Detail>
              <Detail label="Received date">{fmtDate(intake.received_date)}</Detail>
              <Detail label="Entered at">{fmtLocal(intake.entered_at)}</Detail>
              <Detail label="Description">{intake.description}</Detail>
              {intake.missing_items && <Detail label="Waiting for">{intake.missing_items}</Detail>}
              {intake.status_reason && <Detail label="Recorded reason">{intake.status_reason}</Detail>}
              {intake.parent_intake_id && (
                <Detail label="Supplement to">
                  <Link to={`/intakes/${intake.parent_intake_id}`}>the original filing</Link>
                </Detail>
              )}
              {intake.duplicate_of_intake_id && (
                <Detail label="Duplicate of">
                  <Link to={`/intakes/${intake.duplicate_of_intake_id}`}>the original filing</Link>
                </Detail>
              )}
              {data.case && (
                <Detail label="Case">
                  <Link to={`/cases/${data.case.id}`}>
                    {data.case.number} — {data.case.title}
                  </Link>{' '}
                  <StatusBadge status={data.case.status} />
                </Detail>
              )}
              {intake.created_by_name && <Detail label="Recorded by">{intake.created_by_name}</Detail>}
            </tbody>
          </table>
        </div>
      </Card>

      <Card title="Paper original">
        {Boolean(intake.is_paper_original) ? <>
          <p>
            A paper original was received. It is kept at:{' '}
            <strong>{intake.paper_location ?? 'location not recorded'}</strong>
          </p>
          <p className="muted">A scan or copy is not the paper original.</p>
        </> : <p>No paper original received.</p>}
      </Card>

      <Card title="Documents">
        <IntakeDocuments
          intakeId={intake.id}
          documents={data.documents}
          canUpload={hasPerm('intake.manage')}
          onChanged={reload}
        />
      </Card>

      <Card title="Correspondence">
        {data.messages.length === 0 ? (
          <p className="muted">No messages recorded for this filing.</p>
        ) : (
          <ul>
            {data.messages.map((m) => (
              <li key={m.id}>
                <strong>
                  {m.direction === 'incoming' ? 'Received' : m.direction === 'outgoing' ? (m.dispatch_id ? 'Prepared outgoing message' : 'Outgoing message') : 'Note'}
                </strong>{' '}
                <span className="muted">
                  {fmtLocal(m.created_at)}
                  {m.created_by_name ? ` — ${m.created_by_name}` : ''}
                </span>
                <br />
                {m.body}
              </li>
            ))}
          </ul>
        )}
      </Card>

      <Card title="Related filings (supplements and duplicates)">
          <DataTable columns={dupColumns} rows={data.related_intakes} rowKey={(r) => String(r.id)} empty="No related filings." />
        </Card>

      <Card title="Dispatches prepared">
          <DataTable
            columns={[
              { key: 'subject', header: 'Subject' },
              { key: 'recipient_name', header: 'Recipient' },
              { key: 'method', header: 'Method', render: (r: IntakeDispatch) => label(refList(ref, 'dispatch_method'), r.method) },
              { key: 'status', header: 'Status', render: (r: IntakeDispatch) => <StatusBadge status={r.status} /> },
              { key: 'prepared_at', header: 'Prepared', render: (r: IntakeDispatch) => fmtLocal(r.prepared_at) },
            ]}
            rows={data.dispatches}
            rowKey={(r) => String(r.id)}
            empty="No dispatches prepared."
          />
          <p className="muted">
            Prepared messages are reviewed and sent in <Link to="/dispatch">Dispatch</Link>.
          </p>
        </Card>

      {/* ------- action dialogs ------- */}

      <Modal title={`Edit ${intake.reference}`} open={action === 'edit'} onClose={closeAction}>
        <IntakeForm
          initial={intakeFormValues(intake as unknown as Record<string, unknown>)}
          submitLabel="Save changes"
          busy={busy}
          error={actionError}
          attempted={lastAttempted}
          onReviewVersion={(version) => { setEditVersion(version); setActionError(null); }}
          onSubmit={submitEdit}
          onCancel={closeAction}
        />
      </Modal>

      <Modal title={`Add a supplement to ${intake.reference}`} open={action === 'supplement'} onClose={closeAction}>
        <p className="muted">
          The supplement is stored as its own filing, linked to this one. It follows this filing when
          it is registered or linked to a case.
        </p>
        <IntakeForm
          submitLabel="Add supplement"
          busy={busy}
          error={actionError}
          onSubmit={submitSupplement}
          onCancel={closeAction}
        />
      </Modal>

      <Modal title="Request missing information" open={action === 'request_info'} onClose={closeAction}>
        {requestInfoSent ? (
          <>
            <p>A draft message was prepared — nothing has been sent yet.</p>
            <div className="actions">
              {sentDispatchId != null ? (
                <Link to={`/dispatch?dispatch=${sentDispatchId}`} onClick={closeAction}>
                  Review and send now
                </Link>
              ) : (
                <Link to="/dispatch" onClick={closeAction}>Review it in Dispatch</Link>
              )}
              <Button variant="secondary" onClick={closeAction}>Close</Button>
            </div>
          </>
        ) : (
          <>
            <ErrorBanner error={actionError} onRetry={submitRequestInfo} />
            <TextArea
              disabled={busy}
              label="What is missing"
              value={missingItems}
              onChange={setMissingItems}
              required
              rows={4}
              help="Listed plainly — this text goes into the message to the sender."
            />
            <SelectField
              disabled={busy}
              label="Send by"
              value={method}
              onChange={setMethod}
              options={options(refList(ref, 'dispatch_method'))}
            />
            <TextField
              disabled={busy}
              label="Address (optional)"
              value={address}
              onChange={setAddress}
              help="Overrides the recorded contact for this message only."
            />
            <div className="actions">
              <Button busy={busy} disabled={!missingItems.trim()} onClick={submitRequestInfo}>
                Prepare the request
              </Button>
              <Button variant="secondary" onClick={closeAction}>Cancel</Button>
            </div>
          </>
        )}
      </Modal>

      <Modal title="Mark ready for registration" open={action === 'mark_ready'} onClose={closeAction}>
        <ErrorBanner error={actionError} onRetry={submitMarkReady} />
        <p>Administrative check finished — the filing can be registered or linked to a case.</p>
        <TextArea disabled={busy} label="Note (optional)" value={note} onChange={setNote} rows={3} />
        <div className="actions">
          <Button busy={busy} onClick={submitMarkReady}>Mark ready</Button>
          <Button variant="secondary" onClick={closeAction}>Cancel</Button>
        </div>
      </Modal>

      <Modal title="Mark as duplicate" open={action === 'mark_duplicate'} onClose={closeAction}>
        <ErrorBanner error={actionError} onRetry={submitDuplicate} />
        <p className="muted">
          The filing is kept and linked to the original — it is never deleted and never merges records.
        </p>
        <IntakePicker
          label="Original filing"
          value={dupTarget}
          onChange={setDupTarget}
          excludeId={intake.id}
          required
        />
        <TextArea disabled={busy} label="Reason" value={dupReason} onChange={setDupReason} required rows={3} />
        <div className="actions">
          <Button busy={busy} disabled={!dupTarget || !dupReason.trim()} onClick={submitDuplicate}>
            Mark as duplicate
          </Button>
          <Button variant="secondary" onClick={closeAction}>Cancel</Button>
        </div>
      </Modal>

      <ConfirmReasonDialog
        open={action === 'return'}
        title="Return or redirect the filing"
        label="Recorded basis"
        confirmLabel="Return / redirect"
        danger
        busy={busy}
        onConfirm={submitReturn}
        onClose={closeAction}
      />

      <Modal title="Link to an existing case" open={action === 'link'} onClose={closeAction}>
        <ErrorBanner error={actionError} onRetry={submitLink} />
        <p className="muted">
          The filing's documents are attached to the case; the filing keeps its own history.
        </p>
        <CasePicker label="Case" value={linkTarget} onChange={setLinkTarget} required />
        <TextArea disabled={busy} label="Note (optional)" value={linkNote} onChange={setLinkNote} rows={3} />
        <div className="actions">
          <Button busy={busy} disabled={!linkTarget} onClick={submitLink}>
            Link to this case
          </Button>
          <Button variant="secondary" onClick={closeAction}>Cancel</Button>
        </div>
      </Modal>

      <Modal title={`Register ${intake.reference} as a new case`} open={action === 'register'} onClose={closeAction}>
        <RegisterForm
          senderName={intake.sender_name}
          senderPartyId={intake.sender_party_id}
          busy={busy}
          error={actionError}
          onSubmit={submitRegister}
          onCancel={closeAction}
        />
      </Modal>
    </RegistryPage>
  );
}
