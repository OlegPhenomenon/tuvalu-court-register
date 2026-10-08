/**
 * Hearings tab (C07/C08/C10): hearings as cards, newest first, with the
 * adjournment chain kept visible both ways. Actions follow the state machine —
 * draft: edit/confirm/cancel; scheduled: adjourn/cancel/record outcome; held:
 * administrative correction only. A 409 `hearing_conflict` shows the visible
 * clashes and a count of bookings the user may not see.
 */

import { useRef, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';
import { api, ApiError, newKey } from '../../api';
import { useSession } from '../../session';
import { fmtLocal } from '../../time';
import { Button } from '../../components/Button';
import { Card } from '../../components/Card';
import { ErrorBanner } from '../../components/ErrorBanner';
import { DateField, DateTimeField, SelectField, TextArea, TextField } from '../../components/fields';
import { Modal } from '../../components/Modal';
import { StatusBadge } from '../../components/StatusBadge';
import { label, options, refList, useRef as useRefData } from '../../components/refdata';
import { useApi } from '../../components/useApi';
import {
  caseStaffOptions,
  ConflictDetails,
  HearingForm,
  hearingTimeRange,
  judgeOptions,
} from '../../components/HearingForm';
import type { Hearing, HearingConflict } from '../../components/HearingForm';
import type { CaseTabProps } from './types';

function Detail({ term, children }: { term: string; children: ReactNode }) {
  return (
    <tr>
      <th scope="row">{term}</th>
      <td>{children}</td>
    </tr>
  );
}

/** Reason-requiring dialog with an inline error slot (keeps the typed reason on failure). */
function ReasonModal({ title, label: fieldLabel, confirmLabel = 'Confirm', danger, busy, error, onConfirm, onClose }: {
  title: string;
  label: string;
  confirmLabel?: string;
  danger?: boolean;
  busy?: boolean;
  error?: unknown;
  onConfirm: (reason: string) => void;
  onClose: () => void;
}) {
  const [reason, setReason] = useState('');
  return (
    <Modal title={title} open onClose={busy ? () => {} : onClose}>
      <ErrorBanner error={error} onRetry={reason.trim() ? () => onConfirm(reason.trim()) : undefined} />
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
        <Button variant="secondary" disabled={busy} onClick={onClose}>Cancel</Button>
      </div>
    </Modal>
  );
}

/* ------------------------------ confirm a draft ------------------------------ */

function ConfirmHearingModal({ hearing, onClose, onSaved }: {
  hearing: Hearing;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { hasPerm } = useSession();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [conflicts, setConflicts] = useState<{ visible: HearingConflict[]; hidden: number } | null>(null);
  const [reason, setReason] = useState('');

  const go = async (override?: string) => {
    setBusy(true);
    setError(null);
    setConflicts(null);
    try {
      await api('POST', `/hearings/${hearing.id}/confirm`, { override_reason: override ?? null });
      onSaved();
      onClose();
    } catch (e) {
      if (e instanceof ApiError && e.code === 'hearing_conflict') {
        const d = e.details as { conflicts?: HearingConflict[]; hidden_conflicts?: number } | null;
        setConflicts({ visible: d?.conflicts ?? [], hidden: d?.hidden_conflicts ?? 0 });
      }
      setError(e);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal title="Confirm the hearing" open onClose={busy ? () => {} : onClose}>
      <p>
        Confirming books {hearing.room_name ?? 'the room'} and{' '}
        {hearing.judge_name ?? 'the judge'} for {hearingTimeRange(hearing)}. If they are already
        booked you will see the clashing hearings here.
      </p>
      <ErrorBanner error={error && !conflicts ? error : null} onRetry={() => void go()} />
      {conflicts && (
        <>
          <ConflictDetails conflicts={conflicts.visible} hidden={conflicts.hidden} />
          {hasPerm('hearing.override_conflict') && (
            <>
              <TextArea
                label="Reason for booking over the conflict"
                value={reason}
                onChange={setReason}
                required
                rows={2}
                help="Written to the audit log together with your name."
              />
              <div className="actions">
                <Button
                  variant="danger"
                  busy={busy}
                  disabled={!reason.trim()}
                  onClick={() => void go(reason.trim())}
                >
                  Confirm anyway
                </Button>
              </div>
            </>
          )}
        </>
      )}
      <div className="actions">
        <Button busy={busy} onClick={() => void go()}>Confirm</Button>
        <Button variant="secondary" disabled={busy} onClick={onClose}>Back</Button>
      </div>
    </Modal>
  );
}

/* ------------------------------ adjourn ------------------------------ */

function AdjournModal({ hearing, caseData, onClose, onSaved }: {
  hearing: Hearing;
  caseData: CaseTabProps['caseData'];
  onClose: () => void;
  onSaved: () => void;
}) {
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const form = useRef<HTMLFormElement>(null);
  const [idemKey] = useState(() => newKey());
  const judges = caseData.assignments.filter((a) => a.role === 'judge' && a.end_at === null);
  const [start, setStart] = useState(hearing.starts_local);
  const [end, setEnd] = useState(hearing.ends_local);
  const [room, setRoom] = useState(hearing.room_id ? String(hearing.room_id) : '');
  const [judge, setJudge] = useState(hearing.judge_user_id ? String(hearing.judge_user_id) : '');
  const [reason, setReason] = useState('');
  const [authorisedBy, setAuthorisedBy] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [conflicts, setConflicts] = useState<{ visible: HearingConflict[]; hidden: number } | null>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    setConflicts(null);
    try {
      await api('POST', `/hearings/${hearing.id}/adjourn`, {
        starts_local: start,
        ends_local: end,
        room_id: room ? Number(room) : null,
        judge_user_id: judge ? Number(judge) : null,
        reason,
        authorised_by: authorisedBy,
      }, { idempotencyKey: idemKey });
      onSaved();
      onClose();
    } catch (err) {
      if (err instanceof ApiError && err.code === 'hearing_conflict') {
        const d = err.details as { conflicts?: HearingConflict[]; hidden_conflicts?: number } | null;
        setConflicts({ visible: d?.conflicts ?? [], hidden: d?.hidden_conflicts ?? 0 });
      }
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal title="Adjourn the hearing" open onClose={busy ? () => {} : onClose}>
      <p>
        The old hearing stays in the record as <strong>Adjourned</strong>; a new linked hearing is
        created and notification tasks are added for the required participants.
      </p>
      <ErrorBanner error={refError} onRetry={reloadRef} />
      <ErrorBanner error={error && !conflicts ? error : null} onRetry={() => form.current?.requestSubmit()} />
      {conflicts && (
        <>
          <ConflictDetails conflicts={conflicts.visible} hidden={conflicts.hidden} />
          <p className="muted">Pick a different time, room or judge — an adjournment cannot be overridden.</p>
        </>
      )}
      <form ref={form} onSubmit={submit}>
        <fieldset disabled={busy} style={{ border: 0, margin: 0, padding: 0, minWidth: 0 }}>
          <DateTimeField label="New start" value={start} onChange={setStart} required />
          <DateTimeField label="New end" value={end} onChange={setEnd} required />
          <SelectField
            label="Room"
            value={room}
            onChange={setRoom}
            options={(ref?.rooms ?? []).map((r) => ({
              value: String(r.id),
              label: r.location ? `${r.name} — ${r.location}` : r.name,
            }))}
            placeholder="No room"
          />
          <SelectField
            label="Judge"
            value={judge}
            onChange={setJudge}
            options={judgeOptions(judges, hearing.judge_user_id, hearing.judge_name)}
            placeholder="Keep the same judge"
          />
          <TextArea label="Reason for the adjournment" value={reason} onChange={setReason} required rows={3} />
          <TextField
            label="Authorised by"
            value={authorisedBy}
            onChange={setAuthorisedBy}
            required
            help="The person who allowed the move — kept on the record."
          />
          <div className="actions">
            <Button type="submit" busy={busy} disabled={!reason.trim() || !authorisedBy.trim()}>
              Adjourn to the new date
            </Button>
            <Button type="button" variant="secondary" onClick={onClose}>Back</Button>
          </div>
        </fieldset>
      </form>
    </Modal>
  );
}

/* ------------------------------ record outcome ------------------------------ */

function OutcomeModal({ hearing, caseData, onClose, onSaved, onDemoNote }: {
  hearing: Hearing;
  caseData: CaseTabProps['caseData'];
  onClose: () => void;
  onSaved: () => void;
  onDemoNote: (note: string) => void;
}) {
  const { session } = useSession();
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const form = useRef<HTMLFormElement>(null);
  const [held, setHeld] = useState<'yes' | 'no'>('yes');
  const [attendance, setAttendance] = useState<Record<number, boolean>>(() =>
    Object.fromEntries(hearing.participants.map((p) => [p.id, true])),
  );
  const [reason, setReason] = useState('');
  const [summary, setSummary] = useState('');
  const [nextStep, setNextStep] = useState('');
  const [wantTask, setWantTask] = useState(false);
  const [taskTitle, setTaskTitle] = useState('');
  const [taskAssignee, setTaskAssignee] = useState('');
  const [taskDue, setTaskDue] = useState('');
  const [wantNext, setWantNext] = useState(false);
  const [nhType, setNhType] = useState(hearing.hearing_type);
  const [nhStart, setNhStart] = useState('');
  const [nhEnd, setNhEnd] = useState('');
  const [nhRoom, setNhRoom] = useState(hearing.room_id ? String(hearing.room_id) : '');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [conflicts, setConflicts] = useState<{ visible: HearingConflict[]; hidden: number } | null>(null);
  const [localError, setLocalError] = useState('');

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setLocalError('');
    setConflicts(null);
    if (held === 'yes' && !summary.trim()) {
      setLocalError('Write a short outcome summary.');
      return;
    }
    if (held === 'yes' && wantTask && !taskTitle.trim()) {
      setLocalError('The follow-up task needs a title.');
      return;
    }
    if (held === 'yes' && wantNext && (!nhStart || !nhEnd)) {
      setLocalError('Set the start and end of the next hearing, or untick it.');
      return;
    }
    if (held === 'no' && !reason.trim()) {
      setLocalError('State why the hearing did not take place.');
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const res = await api<{ demo_note?: string }>('POST', `/hearings/${hearing.id}/outcome`, {
        held: held === 'yes',
        reason: held === 'no' ? reason : null,
        attendance:
          held === 'yes'
            ? hearing.participants.map((p) => ({ participant_id: p.id, attended: attendance[p.id] ?? false }))
            : [],
        outcome_summary: summary || null,
        next_step: nextStep || null,
        next_task:
          held === 'yes' && wantTask
            ? {
                title: taskTitle,
                assignee_user_id: taskAssignee ? Number(taskAssignee) : null,
                due_date: taskDue || null,
              }
            : null,
        next_hearing:
          held === 'yes' && wantNext
            ? {
                starts_local: nhStart,
                ends_local: nhEnd,
                room_id: nhRoom ? Number(nhRoom) : null,
                hearing_type: nhType || null,
              }
            : null,
      });
      if (res.demo_note) onDemoNote(res.demo_note);
      onSaved();
      onClose();
    } catch (err) {
      if (err instanceof ApiError && err.code === 'hearing_conflict') {
        const d = err.details as { conflicts?: HearingConflict[]; hidden_conflicts?: number } | null;
        setConflicts({ visible: d?.conflicts ?? [], hidden: d?.hidden_conflicts ?? 0 });
      }
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal title={`Record the outcome — ${hearing.hearing_type_label} ${hearingTimeRange(hearing)}`} open onClose={busy ? () => {} : onClose}>
      <ErrorBanner error={refError} onRetry={reloadRef} />
      <ErrorBanner error={error && !conflicts ? error : null} onRetry={() => form.current?.requestSubmit()} />
      {conflicts && (
        <>
          <ConflictDetails conflicts={conflicts.visible} hidden={conflicts.hidden} />
          <p className="muted">The next hearing clashes — change its time or room, or book it later.</p>
        </>
      )}
      {localError && (
        <div className="banner banner--error" role="alert">
          <p>{localError}</p>
        </div>
      )}
      <form ref={form} onSubmit={submit}>
        <fieldset disabled={busy} style={{ border: 0, margin: 0, padding: 0, minWidth: 0 }}>
          <div className="field">
            <span className="field-label">Did the hearing take place?</span>
            <label className="check-row" style={{ marginBottom: '0.25rem' }}>
              <input type="radio" name="held" checked={held === 'yes'} onChange={() => setHeld('yes')} />
              Yes, it was held
            </label>
            <label className="check-row">
              <input type="radio" name="held" checked={held === 'no'} onChange={() => setHeld('no')} />
              No, it did not take place
            </label>
          </div>

          {held === 'no' && (
            <TextArea
              label="Why it did not take place"
              value={reason}
              onChange={setReason}
              required
              rows={3}
              help="Kept on the record together with the date and the notices that were sent."
            />
          )}

          {held === 'yes' && hearing.participants.length > 0 && (
            <div className="field">
              <span className="field-label">Who attended?</span>
              {hearing.participants.map((p) => (
                <label key={p.id} className="check-row" style={{ marginBottom: '0.25rem' }}>
                  <input
                    type="checkbox"
                    checked={attendance[p.id] ?? false}
                    onChange={(e) => setAttendance({ ...attendance, [p.id]: e.target.checked })}
                  />
                  <span>
                    {p.party_name ?? p.user_name}{' '}
                    <span className="muted">
                      ({label(refList(ref, 'participant_role'), p.role)}
                      {p.required ? ', required' : ''})
                    </span>
                  </span>
                </label>
              ))}
            </div>
          )}

          {held === 'yes' && (
            <>
              <TextArea label="Outcome summary" value={summary} onChange={setSummary} required rows={3} />
              <TextArea label="Next step" value={nextStep} onChange={setNextStep} rows={2} />

              <div className="field">
                <label className="check-row">
                  <input type="checkbox" checked={wantTask} onChange={(e) => setWantTask(e.target.checked)} />
                  <span>Add a follow-up task</span>
                </label>
              </div>
              {wantTask && (
                <>
                  <TextField label="Task title" value={taskTitle} onChange={setTaskTitle} required={wantTask} />
                  <SelectField
                    label="Assign to"
                    value={taskAssignee}
                    onChange={setTaskAssignee}
                    options={caseStaffOptions(caseData, session.user)}
                    placeholder="Unassigned"
                  />
                  <DateField label="Due date (set by staff)" value={taskDue} onChange={setTaskDue} />
                </>
              )}

              <div className="field">
                <label className="check-row">
                  <input type="checkbox" checked={wantNext} onChange={(e) => setWantNext(e.target.checked)} />
                  <span>Book the next hearing</span>
                </label>
              </div>
              {wantNext && (
                <>
                  <SelectField
                    label="Type of the next hearing"
                    value={nhType}
                    onChange={setNhType}
                    options={options(refList(ref, 'hearing_type'))}
                    required={wantNext}
                  />
                  <DateTimeField label="Start" value={nhStart} onChange={setNhStart} required={wantNext} />
                  <DateTimeField label="End" value={nhEnd} onChange={setNhEnd} required={wantNext} />
                  <SelectField
                    label="Room"
                    value={nhRoom}
                    onChange={setNhRoom}
                    options={(ref?.rooms ?? []).map((r) => ({
                      value: String(r.id),
                      label: r.location ? `${r.name} — ${r.location}` : r.name,
                    }))}
                    placeholder="No room"
                  />
                  <p className="field-help">
                    The same judge and participants carry over; the new hearing is linked to this one.
                  </p>
                </>
              )}
            </>
          )}

          <div className="actions">
            <Button type="submit" busy={busy}>Record the outcome</Button>
            <Button type="button" variant="secondary" onClick={onClose}>Cancel</Button>
          </div>
          <p className="field-help">
            Recording an outcome never closes the case.
          </p>
        </fieldset>
      </form>
    </Modal>
  );
}

/* ------------------------------ correct a held record ------------------------------ */

function CorrectModal({ hearing, onClose, onSaved }: {
  hearing: Hearing;
  onClose: () => void;
  onSaved: () => void;
}) {
  const form = useRef<HTMLFormElement>(null);
  const [status, setStatus] = useState('scheduled');
  const [reason, setReason] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api('POST', `/hearings/${hearing.id}/correct`, { reason, status });
      onSaved();
      onClose();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal title="Correct the hearing record" open onClose={busy ? () => {} : onClose}>
      <p>
        Use this when the hearing was recorded as held by mistake. Nothing is deleted — the
        correction is written to the audit log with the full previous state.
      </p>
      <ErrorBanner error={error} onRetry={() => form.current?.requestSubmit()} />
      <form ref={form} onSubmit={submit}>
        <fieldset disabled={busy} style={{ border: 0, margin: 0, padding: 0, minWidth: 0 }}>
          <SelectField
            label="Correct the status to"
            value={status}
            onChange={setStatus}
            options={[
              { value: 'scheduled', label: 'Scheduled — it is still to take place' },
              { value: 'cancelled', label: 'Cancelled — it did not take place' },
            ]}
            required
          />
          <TextArea label="Reason for the correction" value={reason} onChange={setReason} required rows={3} />
          <div className="actions">
            <Button type="submit" busy={busy} disabled={!reason.trim()}>Save the correction</Button>
            <Button type="button" variant="secondary" onClick={onClose}>Back</Button>
          </div>
        </fieldset>
      </form>
    </Modal>
  );
}

/* ------------------------------ the tab ------------------------------ */

type ModalState =
  | { kind: 'schedule' }
  | { kind: 'edit'; hearing: Hearing }
  | { kind: 'confirm'; hearing: Hearing }
  | { kind: 'adjourn'; hearing: Hearing }
  | { kind: 'cancel'; hearing: Hearing }
  | { kind: 'outcome'; hearing: Hearing }
  | { kind: 'correct'; hearing: Hearing }
  | null;

export default function HearingsTab({ caseId, caseData, reload }: CaseTabProps) {
  const { hasPerm } = useSession();
  const { data: ref } = useRefData();
  const { data, error, loading, reload: reloadList } = useApi<{ items: Hearing[] }>(
    `/cases/${caseId}/hearings`,
  );
  const allowed = caseData.allowed;
  const [modal, setModal] = useState<ModalState>(null);
  const [modalTick, setModalTick] = useState(0);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<unknown>(null);
  const [demoNote, setDemoNote] = useState<string | null>(null);

  const items = data?.items ?? [];
  const byId = new Map(items.map((h) => [h.id, h]));

  const openModal = (m: NonNullable<ModalState>) => {
    setModalTick((t) => t + 1);
    setActionError(null);
    setModal(m);
  };

  const finish = () => {
    setModal(null);
    reloadList();
    reload();
  };

  const cancel = async (reason: string) => {
    if (modal?.kind !== 'cancel') return;
    setBusy(true);
    setActionError(null);
    try {
      await api('POST', `/hearings/${modal.hearing.id}/cancel`, { reason });
      finish();
    } catch (e) {
      setActionError(e);
    } finally {
      setBusy(false);
    }
  };

  const actionsFor = (h: Hearing): ReactNode => {
    const buttons: ReactNode[] = [];
    if (h.status === 'draft' && allowed.schedule_hearing) {
      buttons.push(
        <Button key="edit" variant="secondary" onClick={() => openModal({ kind: 'edit', hearing: h })}>Edit</Button>,
        <Button key="confirm" onClick={() => openModal({ kind: 'confirm', hearing: h })}>Confirm</Button>,
        <Button key="cancel" variant="secondary" onClick={() => openModal({ kind: 'cancel', hearing: h })}>Cancel</Button>,
      );
    }
    if (h.status === 'scheduled') {
      if (allowed.record_outcome) {
        buttons.push(
          <Button key="outcome" onClick={() => openModal({ kind: 'outcome', hearing: h })}>Record outcome</Button>,
        );
      }
      if (allowed.schedule_hearing) {
        buttons.push(
          <Button key="adjourn" variant="secondary" onClick={() => openModal({ kind: 'adjourn', hearing: h })}>Adjourn</Button>,
          <Button key="cancel" variant="secondary" onClick={() => openModal({ kind: 'cancel', hearing: h })}>Cancel</Button>,
        );
      }
    }
    if (h.status === 'held' && hasPerm('hearing.admin_correct')) {
      buttons.push(
        <Button key="correct" variant="secondary" onClick={() => openModal({ kind: 'correct', hearing: h })}>Correct record</Button>,
      );
    }
    return buttons.length > 0 ? <>{buttons}</> : undefined;
  };

  const participantName = (p: Hearing['participants'][number]) => p.party_name ?? p.user_name ?? '—';

  return (
    <>
      {demoNote && (
        <div
          className="banner"
          role="status"
          style={{ background: '#eef4fd', border: '1px solid #c8d9f3' }}
        >
          <p>{demoNote}</p>
          <div className="banner-actions">
            <Button variant="secondary" onClick={() => setDemoNote(null)}>Understood</Button>
          </div>
        </div>
      )}
      <Card
        title="Hearings"
        actions={
          allowed.schedule_hearing ? (
            <Button onClick={() => openModal({ kind: 'schedule' })}>Schedule hearing</Button>
          ) : undefined
        }
      >
        <p className="muted">
          A draft holds no slot; confirming books the room and the judge. A confirmed hearing never
          moves — it is adjourned into a new linked hearing instead, so both dates stay on record.
        </p>
        <ErrorBanner error={error} onRetry={reloadList} />
        {loading && !data && <p className="muted">Loading…</p>}
        {!loading && items.length === 0 && !error && (
          <p className="muted">No hearings on this case yet.</p>
        )}
      </Card>

      {items.map((h) => {
        const next = h.adjourned_to_id ? byId.get(h.adjourned_to_id) : undefined;
        const prev = h.previous_hearing_id ? byId.get(h.previous_hearing_id) : undefined;
        return (
          <Card
            key={h.id}
            title={
              <>
                {h.hearing_type_label} <StatusBadge status={h.status} />
              </>
            }
            actions={actionsFor(h)}
          >
            <div id={`hearing-${h.id}`}>
              <div className="table-wrap">
                <table>
                  <tbody>
                    <Detail term="When">{hearingTimeRange(h)}</Detail>
                    <Detail term="Room">{h.room_name ?? '—'}</Detail>
                    <Detail term="Judge">{h.judge_name ?? '—'}</Detail>
                    {h.notes && <Detail term="Notes">{h.notes}</Detail>}
                  </tbody>
                </table>
              </div>

              {h.participants.length > 0 && (
                <div className="table-wrap">
                  <table>
                    <thead>
                      <tr>
                        <th scope="col">Participant</th>
                        <th scope="col">Role</th>
                        <th scope="col">Notice</th>
                        {h.outcome_recorded_at && <th scope="col">Attendance</th>}
                      </tr>
                    </thead>
                    <tbody>
                      {h.participants.map((p) => (
                        <tr key={p.id}>
                          <td>{participantName(p)}</td>
                          <td>{label(refList(ref, 'participant_role'), p.role)}</td>
                          <td>{p.required ? 'Required' : 'Optional'}</td>
                          {h.outcome_recorded_at && (
                            <td>
                              {p.attended === null || p.attended === undefined
                                ? '—'
                                : p.attended
                                  ? 'Attended'
                                  : 'Did not attend'}
                            </td>
                          )}
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
              )}

              {h.status === 'adjourned' && (
                <p>
                  Adjourned to{' '}
                  {next ? (
                    <a href={`#hearing-${next.id}`}>{fmtLocal(next.starts_at)}</a>
                  ) : (
                    'a new hearing'
                  )}
                  {h.status_reason ? ` — ${h.status_reason}` : ''}
                  {h.status_authorised_by ? `, authorised by ${h.status_authorised_by}` : ''}
                </p>
              )}
              {h.previous_hearing_id && (
                <p className="muted">
                  Moved here from{' '}
                  {prev ? (
                    <a href={`#hearing-${prev.id}`}>{fmtLocal(prev.starts_at)}</a>
                  ) : (
                    'an earlier hearing'
                  )}
                </p>
              )}
              {h.status === 'cancelled' && h.status_reason && (
                <p className="muted">Cancelled — {h.status_reason}</p>
              )}
              {Boolean(h.conflict_override) && h.override_reason && (
                <p className="muted">
                  Booked over a scheduling conflict — {h.override_reason}
                  {h.override_by_name ? ` (${h.override_by_name})` : ''}
                </p>
              )}

              {h.outcome_summary && <p><strong>Outcome:</strong> {h.outcome_summary}</p>}
              {h.next_step && <p><strong>Next step:</strong> {h.next_step}</p>}
              {h.outcome_recorded_by_name && (
                <p className="muted">
                  Outcome recorded by {h.outcome_recorded_by_name}
                  {h.outcome_recorded_at ? `, ${fmtLocal(h.outcome_recorded_at)}` : ''}
                </p>
              )}
            </div>
          </Card>
        );
      })}

      {(modal?.kind === 'schedule' || modal?.kind === 'edit') && (
        <HearingForm
          key={modalTick}
          caseId={caseId}
          caseData={caseData}
          hearing={modal.kind === 'edit' ? modal.hearing : undefined}
          onClose={() => setModal(null)}
          onSaved={finish}
        />
      )}
      {modal?.kind === 'confirm' && (
        <ConfirmHearingModal key={modalTick} hearing={modal.hearing} onClose={() => setModal(null)} onSaved={finish} />
      )}
      {modal?.kind === 'adjourn' && (
        <AdjournModal key={modalTick} hearing={modal.hearing} caseData={caseData} onClose={() => setModal(null)} onSaved={finish} />
      )}
      {modal?.kind === 'outcome' && (
        <OutcomeModal
          key={modalTick}
          hearing={modal.hearing}
          caseData={caseData}
          onClose={() => setModal(null)}
          onSaved={finish}
          onDemoNote={setDemoNote}
        />
      )}
      {modal?.kind === 'correct' && (
        <CorrectModal key={modalTick} hearing={modal.hearing} onClose={() => setModal(null)} onSaved={finish} />
      )}
      {modal?.kind === 'cancel' && (
        <ReasonModal
          key={modalTick}
          title={`Cancel the hearing on ${fmtLocal(modal.hearing.starts_at)}`}
          label="Reason for cancelling"
          confirmLabel="Cancel the hearing"
          danger
          busy={busy}
          error={actionError}
          onConfirm={(r) => void cancel(r)}
          onClose={() => setModal(null)}
        />
      )}
    </>
  );
}
