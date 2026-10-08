/**
 * Hearing booking form (C07, §8): court-local start/end are sent to the server
 * verbatim, a draft holds no slot, and "Confirm" runs the double-booking check.
 * A 409 `hearing_conflict` lists the visible clashes (plus a count of bookings
 * the user may not see) and can only be booked over with the
 * `hearing.override_conflict` permission and a mandatory reason.
 *
 * The same component edits a draft hearing (PATCH with optimistic locking); a
 * confirmed hearing never moves — it is adjourned instead.
 */

import { useRef, useState } from 'react';
import type { FormEvent } from 'react';
import { Link } from 'react-router-dom';
import { api, ApiError, newKey } from '../api';
import { useSession } from '../session';
import { fmtDate, fmtLocal } from '../time';
import { Button } from './Button';
import { ErrorBanner } from './ErrorBanner';
import { DateTimeField, SelectField, TextArea } from './fields';
import { Modal } from './Modal';
import { label, options, refList, useRef as useRefData } from './refdata';
import type { Assignment, CaseData } from '../pages/case/types';

/* ------------------------- shapes mirroring hearing_json ------------------------- */

export interface HearingParticipant {
  id: number;
  party_id: number | null;
  party_name: string | null;
  user_id: number | null;
  user_name: string | null;
  role: string;
  required: number;
  attended: number | null;
}

export interface Hearing {
  id: number;
  case_id: number;
  case_number: string;
  hearing_type: string;
  hearing_type_label: string;
  status: string;
  starts_at: string;
  ends_at: string;
  starts_local: string;
  ends_local: string;
  room_id: number | null;
  room_name: string | null;
  judge_user_id: number | null;
  judge_name: string | null;
  notes: string | null;
  previous_hearing_id: number | null;
  adjourned_to_id: number | null;
  status_reason: string | null;
  status_authorised_by: string | null;
  conflict_override: number;
  override_reason: string | null;
  override_by_name: string | null;
  outcome_summary: string | null;
  next_step: string | null;
  outcome_recorded_by_name: string | null;
  outcome_recorded_at: string | null;
  created_at: string;
  version: number;
  participants: HearingParticipant[];
}

/** One visible clash inside a 409 `hearing_conflict` details payload. */
export interface HearingConflict {
  hearing_id: number;
  case_id: number;
  case_number: string;
  starts_local: string;
  ends_local: string;
  room_name: string | null;
  judge_name: string | null;
}

/* ------------------------------ shared helpers ------------------------------ */

/** Court-local "YYYY-MM-DDTHH:MM" → "19 Nov 2026, 09:00" (pure string math). */
export function fmtSlot(local: string | null | undefined): string {
  if (!local || local.length < 16) return '—';
  return `${fmtDate(local.slice(0, 10))}, ${local.slice(11, 16)}`;
}

/** Display a hearing's start–end: "Thu 19 Nov 2026, 09:00 – 10:00". */
export function hearingTimeRange(h: Hearing): string {
  return `${fmtLocal(h.starts_at)} – ${h.ends_local.slice(11, 16)}`;
}

/** Select options for the judges currently assigned to the case. */
export function judgeOptions(judges: Assignment[], currentId?: number | null, currentName?: string | null) {
  const opts = judges.map((j) => ({ value: String(j.user_id), label: j.display_name }));
  if (currentId && !opts.some((o) => o.value === String(currentId))) {
    opts.push({ value: String(currentId), label: `${currentName ?? 'Former judge'} — no longer assigned` });
  }
  return opts;
}

/** Select options for staff who currently have access to the case (assignees, responsible, self). */
export function caseStaffOptions(caseData: CaseData, me: { id: string; display_name: string }) {
  const seen = new Map<number, string>();
  for (const a of caseData.assignments) {
    if (!a.end_at && !seen.has(a.user_id)) seen.set(a.user_id, a.display_name);
  }
  const rid = caseData.case.responsible_user_id;
  if (rid && !seen.has(rid)) seen.set(rid, caseData.case.responsible_name ?? `User ${rid}`);
  const meId = Number(me.id);
  if (meId && !seen.has(meId)) seen.set(meId, `${me.display_name} (you)`);
  return [...seen.entries()].map(([id, name]) => ({ value: String(id), label: name }));
}

/** Plain-English rendering of a `hearing_conflict` details payload. */
export function ConflictDetails({ conflicts, hidden }: { conflicts: HearingConflict[]; hidden: number }) {
  return (
    <div className="banner banner--error" role="alert">
      <p><strong>The judge or the room is already booked at that time.</strong></p>
      {conflicts.length > 0 && (
        <ul>
          {conflicts.map((c) => (
            <li key={c.hearing_id}>
              <Link to={`/cases/${c.case_id}?tab=hearings`}>{c.case_number}</Link>{' '}
              {fmtSlot(c.starts_local)} – {c.ends_local.slice(11, 16)}
              {c.room_name ? `, ${c.room_name}` : ''}
              {c.judge_name ? `, ${c.judge_name}` : ''}
            </li>
          ))}
        </ul>
      )}
      {hidden > 0 && <p>…and {hidden} other booking{hidden === 1 ? '' : 's'} you cannot see.</p>}
    </div>
  );
}

/* ------------------------------ the form ------------------------------ */

export function HearingForm({ caseId, caseData, hearing, onClose, onSaved }: {
  caseId: number;
  caseData: CaseData;
  /** Set when editing an existing draft (PATCH); absent → create (POST). */
  hearing?: Hearing;
  onClose: () => void;
  onSaved: () => void;
}) {
  const editing = Boolean(hearing);
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const { hasPerm } = useSession();
  const form = useRef<HTMLFormElement>(null);
  // One Idempotency-Key per opened form — a retry replays, never double-books.
  const [idemKey] = useState(() => newKey());
  const [version, setVersion] = useState(hearing?.version ?? 0);
  const [type, setType] = useState(hearing?.hearing_type ?? '');
  const [start, setStart] = useState(hearing?.starts_local ?? '');
  const [end, setEnd] = useState(hearing?.ends_local ?? '');
  const [room, setRoom] = useState(hearing?.room_id ? String(hearing.room_id) : '');
  const judges = caseData.assignments.filter((a) => a.role === 'judge' && a.end_at === null);
  const [judge, setJudge] = useState(
    hearing?.judge_user_id
      ? String(hearing.judge_user_id)
      : judges[0]
        ? String(judges[0].user_id)
        : '',
  );
  const [notes, setNotes] = useState(hearing?.notes ?? '');
  const participants = caseData.participants.filter((p) => p.active);
  const [picked, setPicked] = useState<Record<number, { in: boolean; required: boolean }>>(() =>
    Object.fromEntries(participants.map((p) => [p.id, { in: true, required: true }])),
  );
  const confirmRef = useRef(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [attempted, setAttempted] = useState<Record<string, unknown> | undefined>();
  const [conflicts, setConflicts] = useState<{ visible: HearingConflict[]; hidden: number } | null>(null);
  const [overrideReason, setOverrideReason] = useState('');
  const canOverride = hasPerm('hearing.override_conflict');

  const submit = async (confirm: boolean, override?: string) => {
    setError(null);
    setConflicts(null);
    const body = editing
      ? {
          version,
          hearing_type: type,
          starts_local: start,
          ends_local: end,
          room_id: room ? Number(room) : null,
          judge_user_id: judge ? Number(judge) : null,
          notes,
        }
      : {
          hearing_type: type,
          starts_local: start,
          ends_local: end,
          room_id: room ? Number(room) : null,
          judge_user_id: judge ? Number(judge) : null,
          notes: notes || null,
          participants: participants
            .filter((p) => picked[p.id]?.in)
            .map((p) => ({ party_id: p.party_id, role: p.role, required: picked[p.id]!.required })),
          confirm,
          override_reason: override ?? null,
        };
    setAttempted(body as Record<string, unknown>);
    setBusy(true);
    try {
      if (editing) await api('PATCH', `/hearings/${hearing!.id}`, body);
      else await api('POST', `/cases/${caseId}/hearings`, body, { idempotencyKey: idemKey });
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

  const onSubmit = (e: FormEvent) => {
    e.preventDefault();
    void submit(confirmRef.current);
  };

  const versionConflict = error instanceof ApiError && error.code === 'version_conflict';
  const currentVersion = versionConflict
    ? (error.details as { current?: { version?: number } })?.current?.version
    : undefined;

  return (
    <Modal
      title={editing ? 'Edit the draft hearing' : 'Schedule a hearing'}
      open
      onClose={busy ? () => {} : onClose}
    >
      <ErrorBanner error={refError} onRetry={reloadRef} />
      <ErrorBanner
        error={error && !conflicts ? error : null}
        attempted={attempted}
        onRetry={error && !conflicts && !versionConflict ? () => form.current?.requestSubmit() : undefined}
      />
      {versionConflict && typeof currentVersion === 'number' && (
        <p>
          <Button
            type="button"
            variant="secondary"
            onClick={() => {
              setVersion(currentVersion);
              setError(null);
            }}
          >
            Keep my edits and use the current version
          </Button>
        </p>
      )}
      {conflicts && (
        <>
          <ConflictDetails conflicts={conflicts.visible} hidden={conflicts.hidden} />
          {canOverride && (
            <>
              <TextArea
                label="Reason for booking over the conflict"
                value={overrideReason}
                onChange={setOverrideReason}
                required
                rows={2}
                help="Written to the audit log together with your name."
              />
              <div className="actions">
                <Button
                  variant="danger"
                  busy={busy}
                  disabled={!overrideReason.trim()}
                  onClick={() => void submit(true, overrideReason.trim())}
                >
                  Confirm anyway
                </Button>
              </div>
            </>
          )}
        </>
      )}
      <form ref={form} onSubmit={onSubmit}>
        <fieldset disabled={busy} style={{ border: 0, margin: 0, padding: 0, minWidth: 0 }}>
          <SelectField
            label="Type of hearing"
            value={type}
            onChange={setType}
            options={options(refList(ref, 'hearing_type'))}
            placeholder="Choose the type"
            required
          />
          <DateTimeField
            label="Start"
            value={start}
            onChange={setStart}
            required
            help="Court time (Pacific/Funafuti), not your computer's timezone."
          />
          <DateTimeField label="End" value={end} onChange={setEnd} required />
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
            options={judgeOptions(judges, hearing?.judge_user_id, hearing?.judge_name)}
            placeholder={judges.length === 0 ? 'No judge assigned to this case' : 'Choose the judge'}
            help="Only a judge assigned to this case can be chosen."
          />
          {!editing && participants.length > 0 && (
            <div className="field">
              <span className="field-label">Participants</span>
              <p className="field-help">
                Tick who takes part. Required participants get a new notice task if the hearing is
                later adjourned.
              </p>
              {participants.map((p) => (
                <div key={p.id} className="check-row" style={{ gap: '1rem', marginBottom: '0.25rem' }}>
                  <label style={{ display: 'flex', gap: '0.5rem', alignItems: 'center' }}>
                    <input
                      type="checkbox"
                      checked={picked[p.id]?.in ?? false}
                      onChange={(e) =>
                        setPicked({
                          ...picked,
                          [p.id]: { in: e.target.checked, required: picked[p.id]?.required ?? true },
                        })
                      }
                    />
                    <span>
                      {p.name}{' '}
                      <span className="muted">({label(refList(ref, 'participant_role'), p.role)})</span>
                    </span>
                  </label>
                  {picked[p.id]?.in && (
                    <label
                      className="muted"
                      style={{ display: 'flex', gap: '0.35rem', alignItems: 'center' }}
                    >
                      <input
                        type="checkbox"
                        checked={picked[p.id]?.required ?? true}
                        onChange={(e) =>
                          setPicked({ ...picked, [p.id]: { in: true, required: e.target.checked } })
                        }
                      />
                      required
                    </label>
                  )}
                </div>
              ))}
            </div>
          )}
          {editing && hearing!.participants.length > 0 && (
            <p className="muted">
              Participants stay as saved ({hearing!.participants.length}). To change them, cancel the
              draft and schedule a new hearing.
            </p>
          )}
          <TextArea label="Notes" value={notes} onChange={setNotes} rows={3} />
          <div className="actions">
            {editing ? (
              <Button type="submit" busy={busy}>Save changes</Button>
            ) : (
              <>
                <Button
                  type="submit"
                  variant="secondary"
                  busy={busy}
                  onClick={() => {
                    confirmRef.current = false;
                  }}
                >
                  Save as draft
                </Button>
                <Button
                  type="submit"
                  busy={busy}
                  onClick={() => {
                    confirmRef.current = true;
                  }}
                >
                  Confirm the booking
                </Button>
              </>
            )}
            <Button type="button" variant="secondary" onClick={onClose}>
              Cancel
            </Button>
          </div>
          {!editing && (
            <p className="field-help">
              A draft holds no slot. Confirming books the room and the judge — if they are already
              booked you will see the clashing hearings.
            </p>
          )}
        </fieldset>
      </form>
    </Modal>
  );
}
