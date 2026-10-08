/**
 * DispatchCard — one notice or copy package with its four separate records
 * (C09/C12; spec §3 step 5, §7 "раздельные состояния"):
 *   1. who prepared/reviewed/queued it,
 *   2. technical delivery attempts (mail server receipts),
 *   3. handover confirmed by a person,
 *   4. the legal assessment of service — recorded by an authorised person,
 *      never decided by the mail server.
 * Shared by the case Dispatch tab and the cross-case Dispatch page.
 */

import { useEffect, useRef, useState } from 'react';
import { Link } from 'react-router-dom';
import { api, downloadUrl, newKey } from '../api';
import { Button } from './Button';
import { Card } from './Card';
import { ErrorBanner } from './ErrorBanner';
import { DateField, SelectField, TextArea } from './fields';
import { Modal } from './Modal';
import { StatusBadge } from './StatusBadge';
import { label, refList, useRef as useRefData } from './refdata';
import { DispatchForm } from './DispatchForm';
import { fmtDate, fmtLocal, courtToday } from '../time';
import { useSession } from '../session';

/* ------------------------------------------------------------------ types */

export interface DispatchItem {
  document_version_id: number;
  document_id: number;
  document_title: string;
  version_no: number;
  filename: string;
  sha256: string;
  visibility: string;
  material_kind: 'working_document' | 'decision_copy';
}

export interface DispatchAttempt {
  attempt_no: number;
  status: string;
  technical_receipt: string | null;
  detail: string | null;
  occurred_date: string | null;
  at: string;
}

export interface DispatchConfirmation {
  kind: string; // 'technical_ack' | 'human_handover'
  note: string;
  occurred_date: string | null;
  recorded_by_name: string | null;
  recorded_at: string;
}

export interface ServiceAssessment {
  assessment: string; // 'served' | 'not_served' | 'undetermined'
  basis: string;
  assessed_by_name: string | null;
  assessed_at: string;
}

/** Full dispatch JSON — GET /api/dispatches/{id} (dispatch.rs `dispatch_json`). */
export interface DispatchRecord {
  id: number;
  case_id: number | null;
  case_number: string | null;
  intake_id: number | null;
  intake_reference: string | null;
  hearing_id: number | null;
  kind: string; // 'notice' | 'copies' | 'information_request'
  template_code: string | null;
  recipient_party_id: number | null;
  recipient_name: string;
  method: string;
  address: string | null;
  subject: string;
  body: string;
  purpose: string | null;
  status: string; // draft | queued | sent | failed | cancelled
  reviewed_by_name: string | null;
  reviewed_at: string | null;
  prepared_by_name: string | null;
  prepared_at: string;
  queued_by_name: string | null;
  queued_at: string | null;
  sent_at: string | null;
  failure_reason: string | null;
  status_reason: string | null;
  version: number;
  items: DispatchItem[];
  attempts: DispatchAttempt[];
  confirmations: DispatchConfirmation[];
  assessments: ServiceAssessment[];
  mailbox_ids: number[];
  state_summary: string;
}

export const DISPATCH_KIND_LABELS: Record<string, string> = {
  notice: 'Notice',
  copies: 'Copy package',
  information_request: 'Information request',
};

const CONFIRM_KIND_LABELS: Record<string, string> = {
  human_handover: 'Handed over to / received by a person',
  technical_ack: 'Technical acknowledgement',
};

const ASSESSMENT_LABELS: Record<string, string> = {
  served: 'Served',
  not_served: 'Not served',
  undetermined: 'Undetermined',
};

const VISIBILITY_LABELS: Record<string, string> = {
  administrative: 'Administrative',
  party_material: 'Party material',
  restricted: 'Restricted',
  judicial_note: 'Judicial note',
};

/** Poll GET /api/dispatches/{id} after queueing — 1s, 2s, 4s — until it leaves `queued`. */
const POLL_DELAYS_MS = [1000, 2000, 4000];

type ActionKind = 'preview' | 'sent' | 'confirm' | 'assess' | 'cancel' | 'edit' | null;

/** Reason dialog with an inline error slot — a failed cancel keeps the typed reason visible. */
function ReasonModal({ open, title, label: fieldLabel, confirmLabel, danger, busy, error, onConfirm, onClose }: {
  open: boolean;
  title: string;
  label: string;
  confirmLabel: string;
  danger?: boolean;
  busy?: boolean;
  error?: unknown;
  onConfirm: (reason: string) => void;
  onClose: () => void;
}) {
  const [reason, setReason] = useState('');
  return (
    <Modal title={title} open={open} onClose={busy ? () => {} : onClose}>
      <ErrorBanner error={error} onRetry={() => onConfirm(reason.trim())} />
      <TextArea disabled={busy} label={fieldLabel} value={reason} onChange={setReason} required rows={3} autoFocus />
      <div className="actions">
        <Button
          variant={danger ? 'danger' : 'primary'}
          busy={busy}
          disabled={!reason.trim()}
          onClick={() => onConfirm(reason.trim())}
        >
          {confirmLabel}
        </Button>
        <Button variant="secondary" disabled={busy} onClick={onClose}>Back</Button>
      </div>
    </Modal>
  );
}

export function DispatchCard({ dispatch, showContext, onChanged }: {
  dispatch: DispatchRecord;
  /** Cross-case page: show the owning case / filing link. */
  showContext?: boolean;
  /** Parent re-fetches its list after a mutation. */
  onChanged: () => void;
}) {
  const { hasPerm } = useSession();
  const { data: ref } = useRefData();
  const [d, setD] = useState(dispatch);
  const [action, setAction] = useState<ActionKind>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [polling, setPolling] = useState(false);
  const pollTimers = useRef<ReturnType<typeof setTimeout>[]>([]);
  // One idempotency key per queue attempt — a network retry replays the same
  // operation; a successful queue rotates the key for the next time.
  const [commandKey, setCommandKey] = useState(newKey);
  const [queueKey, setQueueKey] = useState(() => newKey());

  // Record-sent / confirm / assess form state
  const [occurredDate, setOccurredDate] = useState(courtToday());
  const [note, setNote] = useState('');
  const [confirmKind, setConfirmKind] = useState('human_handover');
  const [assessment, setAssessment] = useState('served');
  const [basis, setBasis] = useState('');

  useEffect(() => setD(dispatch), [dispatch]);
  useEffect(() => () => pollTimers.current.forEach(clearTimeout), []);

  // Case dispatches use dispatch.manage; intake information requests use intake.manage
  // (dispatch.rs require_dispatch_perm). The legal assessment is its own permission.
  const canManage = d.case_id !== null ? hasPerm('dispatch.manage') : hasPerm('intake.manage');
  const canAssess = hasPerm('dispatch.assess_service');

  const open = (a: NonNullable<ActionKind>) => {
    setCommandKey(newKey());
    setAction(a);
    setError(null);
    setOccurredDate(courtToday());
    setNote('');
    setConfirmKind('human_handover');
    setAssessment('served');
    setBasis('');
  };
  const close = () => {
    if (!busy) {
      setAction(null);
      setError(null);
    }
  };

  const run = async (fn: () => Promise<DispatchRecord | void>) => {
    setBusy(true);
    setError(null);
    try {
      const fresh = await fn();
      if (fresh) setD(fresh);
      setAction(null);
      onChanged();
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };

  const refresh = (fresh: DispatchRecord) => {
    setD(fresh);
    onChanged();
  };

  /** After queueing, the outbox worker delivers within moments — poll a few times. */
  const pollUntilSettled = () => {
    setPolling(true);
    let i = 0;
    const step = () => {
      void (async () => {
        try {
          const fresh = await api<DispatchRecord>('GET', `/dispatches/${d.id}`);
          if (fresh.status === 'queued' && i < POLL_DELAYS_MS.length) {
            pollTimers.current.push(setTimeout(step, POLL_DELAYS_MS[i++]));
            return;
          }
          setD(fresh);
        } catch {
          // A failed poll still ends with a parent reload below.
        } finally {
          // Every settled (or last) check refreshes the surrounding list too.
        }
        setPolling(false);
        onChanged();
      })();
    };
    pollTimers.current.push(setTimeout(step, POLL_DELAYS_MS[i++]));
  };

  const send = () =>
    run(async () => {
      const fresh = await api<DispatchRecord>('POST', `/dispatches/${d.id}/queue`, {}, { idempotencyKey: queueKey });
      setQueueKey(newKey());
      setD(fresh);
      if (fresh.status === 'queued') pollUntilSettled();
    });

  const retry = () =>
    run(async () => {
      const fresh = await api<DispatchRecord>('POST', `/dispatches/${d.id}/retry`);
      if (fresh.status === 'queued') pollUntilSettled();
      return fresh;
    });

  const recordSent = () =>
    run(() =>
      api<DispatchRecord>('POST', `/dispatches/${d.id}/record-sent`, {
        occurred_date: occurredDate,
        note,
      }, { idempotencyKey: commandKey }),
    );

  const confirmHandover = () =>
    run(() =>
      api<DispatchRecord>('POST', `/dispatches/${d.id}/confirm`, {
        kind: confirmKind,
        note,
        occurred_date: occurredDate || null,
      }, { idempotencyKey: commandKey }),
    );

  const assessService = () =>
    run(() =>
      api<DispatchRecord>('POST', `/dispatches/${d.id}/assess`, {
        assessment,
        basis,
      }),
    );

  const cancel = (reason: string) =>
    run(() => api<DispatchRecord>('POST', `/dispatches/${d.id}/cancel`, { reason }));

  const confirmReview = () =>
    run(() => api<DispatchRecord>('POST', `/dispatches/${d.id}/preview`));

  const isDraft = d.status === 'draft';
  const isFailed = d.status === 'failed';
  const isSent = d.status === 'sent';
  const isQueued = d.status === 'queued';
  const reviewed = d.reviewed_at !== null;
  const isEmail = d.method === 'email';
  const handed = d.confirmations.some((c) => c.kind === 'human_handover');

  return (
    <Card
      title={
        <>
          {DISPATCH_KIND_LABELS[d.kind] ?? d.kind} to {d.recipient_name}{' '}
          <StatusBadge status={d.status} />
        </>
      }
      actions={
        canManage ? (
          <div className="page-actions">
            {isDraft && (
              <>
                <Button variant="secondary" onClick={() => open('edit')}>Edit</Button>
                <Button variant="secondary" onClick={() => open('preview')}>Review contents</Button>
                {isEmail ? (
                  <Button
                    busy={busy || polling}
                    disabled={!reviewed}
                    title={reviewed ? undefined : 'Review the contents first'}
                    onClick={send}
                  >
                    Send
                  </Button>
                ) : (
                  <Button
                    disabled={!reviewed}
                    title={reviewed ? undefined : 'Review the contents first'}
                    onClick={() => open('sent')}
                  >
                    Record as sent
                  </Button>
                )}
              </>
            )}
            {isFailed && (
              <>
                <Button variant="secondary" onClick={() => open('preview')}>Review contents</Button>
                {isEmail && <Button busy={busy} disabled={!reviewed}
                  title={reviewed ? undefined : 'Review the contents first'} onClick={retry}>Retry</Button>}
              </>
            )}
            {isSent && (
              <Button variant="secondary" onClick={() => open('confirm')}>Confirm handover</Button>
            )}
            {(isDraft || isQueued || isFailed) && (
              <Button variant="danger" onClick={() => open('cancel')}>Cancel</Button>
            )}
          </div>
        ) : undefined
      }
    >
      <p className="dispatch-headline">{d.state_summary}</p>
      {polling && <p className="muted" role="status">Waiting for the local outbox…</p>}

      <div className="table-wrap">
        <table>
          <tbody>
            {showContext && d.case_id !== null && (
              <tr>
                <th scope="row">Case</th>
                <td><Link to={`/cases/${d.case_id}?tab=dispatch`}>{d.case_number}</Link></td>
              </tr>
            )}
            {showContext && d.intake_id !== null && (
              <tr>
                <th scope="row">Filing</th>
                <td><Link to={`/intakes/${d.intake_id}`}>{d.intake_reference}</Link></td>
              </tr>
            )}
            <tr>
              <th scope="row">Recipient</th>
              <td>{d.recipient_name}</td>
            </tr>
            <tr>
              <th scope="row">Method</th>
              <td>{label(refList(ref, 'dispatch_method'), d.method)}</td>
            </tr>
            <tr>
              <th scope="row">Address</th>
              <td>{d.address ?? '—'}</td>
            </tr>
            <tr>
              <th scope="row">Subject</th>
              <td>{d.subject}</td>
            </tr>
            {d.purpose && (
              <tr>
                <th scope="row">Purpose</th>
                <td>{d.purpose}</td>
              </tr>
            )}
            {d.status_reason && (
              <tr>
                <th scope="row">Recorded reason</th>
                <td>{d.status_reason}</td>
              </tr>
            )}
          </tbody>
        </table>
      </div>

      {d.items.length > 0 && (
        <div className="dispatch-section">
          <h3>Contents — exact versions</h3>
          <ul className="dispatch-items">
            {d.items.map((it) => (
              <li key={it.document_version_id}>
                <strong>{it.document_title}</strong> <span className="muted">{it.material_kind === 'decision_copy' ? 'Copy of finalised decision' : 'DRAFT / working material'}</span> — version {it.version_no},{' '}
                <a href={downloadUrl(`/document-versions/${it.document_version_id}/download`)} download>
                  {it.filename}
                </a>{' '}
                <span className="muted">({(VISIBILITY_LABELS[it.visibility] ?? it.visibility).toLowerCase()})</span>
              </li>
            ))}
          </ul>
        </div>
      )}

      {d.mailbox_ids.length > 0 && (
        <p>
          <Link to={`/mailbox?dispatch=${d.id}`}>See the delivered copy in the local mailbox</Link>
        </p>
      )}

      <div className="dispatch-grid">
        <div className="dispatch-section">
          <h3>Prepared / reviewed</h3>
          <ul className="dispatch-facts">
            <li>
              Prepared by {d.prepared_by_name ?? '—'} · {fmtLocal(d.prepared_at)}
            </li>
            <li>
              {reviewed
                ? <>Reviewed by {d.reviewed_by_name ?? '—'} · {fmtLocal(d.reviewed_at!)}</>
                : <span className="muted">Not reviewed yet — sending needs a human contents check.</span>}
            </li>
            {d.queued_at && (
              <li>Queued by {d.queued_by_name ?? '—'} · {fmtLocal(d.queued_at)}</li>
            )}
            {d.sent_at && <li>Recorded as sent · {fmtLocal(d.sent_at)}</li>}
          </ul>
        </div>

        <div className="dispatch-section">
          <h3>Technical delivery</h3>
          {d.attempts.length === 0 ? (
            <p className="muted">No delivery attempts yet.</p>
          ) : (
            <ul className="dispatch-facts">
              {d.attempts.map((a) => (
                <li key={a.attempt_no}>
                  Attempt {a.attempt_no} — <StatusBadge status={a.status} />{' '}
                  <span className="muted">
                    {fmtLocal(a.at)}
                    {a.occurred_date ? ` · occurred ${fmtDate(a.occurred_date)}` : ''}
                    {a.technical_receipt === 'manual'
                      ? ' · recorded by hand'
                      : a.technical_receipt
                        ? ` · receipt ${a.technical_receipt}`
                        : ''}
                  </span>
                  {a.detail && <div>{a.detail}</div>}
                </li>
              ))}
            </ul>
          )}
        </div>

        <div className="dispatch-section">
          <h3>Handover confirmed by a person</h3>
          {d.confirmations.length === 0 ? (
            <p className="muted">No handover confirmations recorded.</p>
          ) : (
            <ul className="dispatch-facts">
              {d.confirmations.map((c, i) => (
                <li key={i}>
                  {CONFIRM_KIND_LABELS[c.kind] ?? c.kind.replace(/_/g, ' ')} — {c.note}
                  <div className="muted">
                    {c.occurred_date ? `occurred ${fmtDate(c.occurred_date)} · ` : ''}
                    recorded by {c.recorded_by_name ?? '—'} · {fmtLocal(c.recorded_at)}
                  </div>
                </li>
              ))}
            </ul>
          )}
          {isSent && !handed && (
            <p className="muted">Sent — waiting for confirmation that a person received it.</p>
          )}
        </div>

        <div className="dispatch-section">
          <h3>Legal assessment of service</h3>
          <p className="muted">
            Recorded by an authorised person. The mail server does not decide proper service.
          </p>
          {d.assessments.length === 0 ? (
            <p className="muted">No service assessment recorded.</p>
          ) : (
            <ul className="dispatch-facts">
              {d.assessments.map((a, i) => (
                <li key={i}>
                  <strong>{ASSESSMENT_LABELS[a.assessment] ?? a.assessment}</strong> — {a.basis}
                  <div className="muted">
                    assessed by {a.assessed_by_name ?? '—'} · {fmtLocal(a.assessed_at)}
                  </div>
                </li>
              ))}
            </ul>
          )}
          {isSent && canAssess && (
            <Button variant="secondary" onClick={() => open('assess')}>
              Record service assessment
            </Button>
          )}
        </div>
      </div>

      {!action && <ErrorBanner error={error} />}

      {/* ---------------- action dialogs ---------------- */}

      <Modal title="Review the exact contents before sending" open={action === 'preview'} onClose={close}>
        <p className="muted">
          This is exactly what the recipient gets. Check the recipient, the address and every
          listed version — sending is only possible after this check.
        </p>
        <ErrorBanner error={error} onRetry={confirmReview} />
        <div className="dispatch-preview">
          <p><strong>To:</strong> {d.recipient_name}{d.address ? ` — ${d.address}` : ''}</p>
          <p><strong>Method:</strong> {label(refList(ref, 'dispatch_method'), d.method)}</p>
          <p><strong>Subject:</strong> {d.subject}</p>
          <pre className="dispatch-body">{d.body}</pre>
          {d.items.length > 0 && (
            <ul className="dispatch-items">
              {d.items.map((it) => (
                <li key={it.document_version_id}>
                  {it.document_title} — {it.material_kind === 'decision_copy' ? 'Copy of finalised decision' : 'DRAFT / working material'} — version {it.version_no} ({it.filename})
                </li>
              ))}
            </ul>
          )}
        </div>
        <div className="actions">
          <Button busy={busy} onClick={confirmReview}>Confirm — I checked this</Button>
          <Button variant="secondary" onClick={close}>Back</Button>
        </div>
      </Modal>

      <Modal title={`Record as sent — ${d.recipient_name}`} open={action === 'sent'} onClose={close}>
        <p className="muted">
          For post, hand delivery, collection or an island court officer. This records that the
          reviewed package physically left; a person still confirms the handover separately.
        </p>
        <ErrorBanner error={error} onRetry={recordSent} />
        <DateField
          label="Date it was sent / handed over"
          value={occurredDate}
          onChange={setOccurredDate}
          required
        />
        <TextArea
          label="Note"
          value={note}
          onChange={setNote}
          required
          rows={3}
          help="Who took it, where, any receipt or tracking detail."
        />
        <div className="actions">
          <Button busy={busy} disabled={!occurredDate || !note.trim()} onClick={recordSent}>
            Record as sent
          </Button>
          <Button variant="secondary" onClick={close}>Cancel</Button>
        </div>
      </Modal>

      <Modal title={`Confirm handover — ${d.recipient_name}`} open={action === 'confirm'} onClose={close}>
        <p className="muted">
          A person confirms the package reached the recipient — separate from the technical
          delivery attempt.
        </p>
        <ErrorBanner error={error} onRetry={confirmHandover} />
        <SelectField
          label="Kind of confirmation"
          value={confirmKind}
          onChange={setConfirmKind}
          options={[
            { value: 'human_handover', label: 'Handed over to / received by a person' },
            { value: 'technical_ack', label: 'Technical acknowledgement' },
          ]}
          required
        />
        <DateField
          label="Date it happened (optional)"
          value={occurredDate}
          onChange={setOccurredDate}
        />
        <TextArea label="Note" value={note} onChange={setNote} required rows={3}
          help="Who confirmed it and how (signature, phone call, registry log)." />
        <div className="actions">
          <Button busy={busy} disabled={!note.trim()} onClick={confirmHandover}>
            Record confirmation
          </Button>
          <Button variant="secondary" onClick={close}>Cancel</Button>
        </div>
      </Modal>

      <Modal title={`Legal assessment of service — ${d.recipient_name}`} open={action === 'assess'} onClose={close}>
        <p className="muted">
          Recorded by an authorised person. The mail server does not decide proper service.
        </p>
        <ErrorBanner error={error} onRetry={assessService} />
        <SelectField
          label="Assessment"
          value={assessment}
          onChange={setAssessment}
          options={[
            { value: 'served', label: 'Served' },
            { value: 'not_served', label: 'Not served' },
            { value: 'undetermined', label: 'Undetermined' },
          ]}
          required
        />
        <TextArea label="Basis" value={basis} onChange={setBasis} required rows={3}
          help="What the assessment rests on (receipt, confirmation, refusal…)." />
        <div className="actions">
          <Button busy={busy} disabled={!basis.trim()} onClick={assessService}>
            Record assessment
          </Button>
          <Button variant="secondary" onClick={close}>Cancel</Button>
        </div>
      </Modal>

      <ReasonModal
        open={action === 'cancel'}
        title={`Cancel this dispatch to ${d.recipient_name}`}
        label="Reason"
        confirmLabel="Cancel the dispatch"
        danger
        busy={busy}
        error={error}
        onConfirm={cancel}
        onClose={close}
      />

      {action === 'edit' && (
        <DispatchForm
          dispatch={d}
          onClose={close}
          onSaved={(fresh) => {
            setAction(null);
            if (fresh) refresh(fresh);
            else onChanged();
          }}
        />
      )}
    </Card>
  );
}
