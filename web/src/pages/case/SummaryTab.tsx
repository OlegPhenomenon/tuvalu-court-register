/**
 * Summary tab (C03/C05/C13): case card with edit, registry-status actions
 * (active / on hold / close / reopen), assignments, relations, status history
 * and linked filings. All mutations go through api.ts; edits use optimistic
 * locking (`version`), close uses an Idempotency-Key.
 */

import { useRef, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';
import { Link, useNavigate } from 'react-router-dom';
import { api, newKey, ApiError } from '../../api';
import { Button } from '../../components/Button';
import { Card } from '../../components/Card';
import { DataTable } from '../../components/DataTable';
import type { Column } from '../../components/DataTable';
import { ErrorBanner } from '../../components/ErrorBanner';
import { CheckboxField, DateField, SelectField, TextArea, TextField } from '../../components/fields';
import { Modal } from '../../components/Modal';
import { StatusBadge } from '../../components/StatusBadge';
import { label, options, refList, staffOptions, useRef as useRefData } from '../../components/refdata';
import { fmtDate, fmtLocal, courtToday } from '../../time';
import { FormErrors } from '../intake/FormErrors';
import { CasePicker } from '../intake/pickers';
import type { CaseHit } from '../intake/pickers';
import type { Assignment, CaseData, CaseTabProps, OpenItem } from './types';

function Detail({ term, children }: { term: string; children: ReactNode }) {
  return (
    <tr>
      <th scope="row">{term}</th>
      <td>{children}</td>
    </tr>
  );
}

const ASSIGN_ROLE_LABELS: Record<string, string> = {
  judge: 'Judge',
  clerk: 'Clerk',
  service_officer: 'Service officer',
  registry_head: 'Registry head',
  other: 'Other',
};

const OPEN_ITEM_TAB: Record<string, string> = {
  task: 'tasks',
  hearing: 'hearings',
  dispatch: 'dispatch',
  decision: 'decisions',
};

/** Reason-requiring confirm dialog with an inline error slot (keeps the typed reason on failure). */
function ReasonModal({ open, title, label: fieldLabel, confirmLabel = 'Confirm', danger, busy, error, onConfirm, onClose }: {
  open: boolean;
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
        <Button variant="secondary" disabled={busy} onClick={onClose}>Cancel</Button>
      </div>
    </Modal>
  );
}

/* ------------------------------ edit form ------------------------------ */

function EditCaseModal({ caseData, onClose, onSaved }: {
  caseData: CaseData;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const c = caseData.case;
  const form = useRef<HTMLFormElement>(null);
  const [version, setVersion] = useState(c.version);
  const [title, setTitle] = useState(c.title);
  const [category, setCategory] = useState(c.category);
  const [summary, setSummary] = useState(c.summary ?? '');
  const [responsible, setResponsible] = useState(c.responsible_user_id ? String(c.responsible_user_id) : '');
  const [restricted, setRestricted] = useState(Boolean(c.restricted));
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [attempted, setAttempted] = useState<Record<string, unknown> | undefined>(undefined);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const body = {
      version,
      title,
      category,
      summary,
      responsible_user_id: responsible ? Number(responsible) : null,
      restricted,
    };
    setAttempted(body as Record<string, unknown>);
    setBusy(true);
    setError(null);
    try {
      await api('PATCH', `/cases/${c.id}`, body);
      onSaved();
      onClose();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal title={`Edit case ${c.number}`} open onClose={busy ? () => {} : onClose}>
      <FormErrors error={error} form={form} attempted={attempted}
        onReviewVersion={(version) => { setVersion(version); setError(null); }} />
      <ErrorBanner error={refError} onRetry={reloadRef} />
      <form ref={form} onSubmit={submit}>
      <fieldset disabled={busy} style={{ border: 0, margin: 0, padding: 0, minWidth: 0 }}>
        <TextField label="Title" value={title} onChange={setTitle} required />
        <SelectField
          label="Category"
          value={category}
          onChange={setCategory}
          options={options(refList(ref, 'case_category'))}
          required
        />
        <TextArea label="Summary" value={summary} onChange={setSummary} rows={3} />
        <SelectField
          label="Responsible officer"
          value={responsible}
          onChange={setResponsible}
          options={staffOptions(ref?.staff)}
          placeholder={c.responsible_user_id ? "Keep current responsible officer" : "Not assigned"}
          required={Boolean(c.responsible_user_id)}
        />
        <CheckboxField
          label="Restricted case"
          checked={restricted}
          onChange={setRestricted}
          help="Hidden from lists, search and counts for staff without the right to see it."
        />
        <div className="actions">
          <Button type="submit" busy={busy}>Save changes</Button>
          <Button type="button" variant="secondary" onClick={onClose}>Cancel</Button>
        </div>
      </fieldset>
      </form>
    </Modal>
  );
}

/* ------------------------------ close form ------------------------------ */

function CloseCaseModal({ caseId, onClose, onSaved }: {
  caseId: number;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const form = useRef<HTMLFormElement>(null);
  const [basis, setBasis] = useState('');
  const [note, setNote] = useState('');
  const [closedDate, setClosedDate] = useState(courtToday());
  // One key per opened form — a network retry replays, never double-closes.
  const [idemKey] = useState(() => newKey());
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [openItems, setOpenItems] = useState<OpenItem[] | null>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    setOpenItems(null);
    try {
      await api('POST', `/cases/${caseId}/close`, {
        basis,
        note: note || null,
        closed_date: closedDate || null,
      }, { idempotencyKey: idemKey });
      onSaved();
      onClose();
    } catch (err) {
      if (err instanceof ApiError && err.code === 'open_items') {
        const items = (err.details as { items?: OpenItem[] } | null)?.items;
        setOpenItems(items ?? []);
      }
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal title="Close the case" open onClose={busy ? () => {} : onClose}>
      <ErrorBanner error={refError} onRetry={reloadRef} />
      <FormErrors error={error} form={form} />
      <form ref={form} onSubmit={submit}>
      <fieldset disabled={busy} style={{ border: 0, margin: 0, padding: 0, minWidth: 0 }}>
        {openItems && (
          <div className="banner" role="alert" style={{ background: '#fdeacc', border: '1px solid #ecc97e', color: '#5d3c00' }}>
            <p>
              <strong>The case cannot be closed yet.</strong> Complete these, cancel them with a
              reason, or carry tasks forward — then close again.
            </p>
            <ul>
              {openItems.map((it) => (
                <li key={`${it.kind}-${it.id}`}>
                  <Link onClick={onClose} to={`?tab=${OPEN_ITEM_TAB[it.kind] ?? 'summary'}`}>{it.label}</Link>{' '}
                  <StatusBadge status={it.status} />
                </li>
              ))}
            </ul>
          </div>
        )}
        <SelectField
          label="Basis for closing"
          value={basis}
          onChange={setBasis}
          options={options(refList(ref, 'closure_basis'))}
          placeholder="Choose the basis"
          required
        />
        <TextArea
          label="Note"
          value={note}
          onChange={setNote}
          rows={3}
          required={basis === 'other'}
          help={basis === 'other' ? 'Required when the basis is "other".' : undefined}
        />
        <DateField label="Closed date" value={closedDate} onChange={setClosedDate} required />
        <p className="muted">
          Closed in the register means the registry stage is complete. It is not proof that the
          decision was enforced or that appeal rights have expired.
        </p>
        <div className="actions">
          <Button type="submit" variant="danger" busy={busy} disabled={!basis}>
            Close the case
          </Button>
          <Button type="button" variant="secondary" onClick={onClose}>Cancel</Button>
        </div>
      </fieldset>
      </form>
    </Modal>
  );
}

/* ------------------------------ assign form ------------------------------ */

function AssignModal({ caseId, allowed, staff, onClose, onSaved }: {
  caseId: number;
  allowed: CaseData['allowed'];
  staff: { id: number; display_name: string; title: string | null; is_judge: number | boolean }[];
  onClose: () => void;
  onSaved: () => void;
}) {
  const form = useRef<HTMLFormElement>(null);
  const roles = [
    ...(allowed.assign_judge ? [{ value: 'judge', label: 'Judge' }] : []),
    ...(allowed.assign_staff
      ? [
          { value: 'clerk', label: 'Clerk' },
          { value: 'service_officer', label: 'Service officer' },
          { value: 'registry_head', label: 'Registry head' },
          { value: 'other', label: 'Other' },
        ]
      : []),
  ];
  const [role, setRole] = useState(roles[0]?.value ?? 'clerk');
  const [userId, setUserId] = useState('');
  const [why, setWhy] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  // The judge role only accepts registered judicial officers.
  const eligible = role === 'judge' ? staff.filter((s) => s.is_judge) : staff;

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api('POST', `/cases/${caseId}/assignments`, {
        user_id: Number(userId),
        role,
        reason: why,
      });
      onSaved();
      onClose();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal title="Assign to this case" open onClose={busy ? () => {} : onClose}>
      <FormErrors error={error} form={form} />
      <form ref={form} onSubmit={submit}>
      <fieldset disabled={busy} style={{ border: 0, margin: 0, padding: 0, minWidth: 0 }}>
        <SelectField
          label="Role"
          value={role}
          onChange={(r) => {
            setRole(r);
            setUserId('');
          }}
          options={roles}
          required
        />
        <SelectField
          label="Person"
          value={userId}
          onChange={setUserId}
          options={staffOptions(eligible)}
          placeholder={role === 'judge' ? 'Judicial officers only' : 'Choose a staff member'}
          required
        />
        <TextArea label="Reason" value={why} onChange={setWhy} required rows={3} />
        <div className="actions">
          <Button type="submit" busy={busy} disabled={!userId || !why.trim()}>Assign</Button>
          <Button type="button" variant="secondary" onClick={onClose}>Cancel</Button>
        </div>
      </fieldset>
      </form>
    </Modal>
  );
}

/* ------------------------------ relations ------------------------------ */

function AddRelationModal({ caseId, onClose, onSaved }: {
  caseId: number;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const form = useRef<HTMLFormElement>(null);
  const [other, setOther] = useState<CaseHit | null>(null);
  const [kind, setKind] = useState('');
  const [note, setNote] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api('POST', `/cases/${caseId}/relations`, {
        to_case_id: other?.id,
        kind,
        note: note || null,
      });
      onSaved();
      onClose();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal title="Add a case relation" open onClose={busy ? () => {} : onClose}>
      <ErrorBanner error={refError} onRetry={reloadRef} />
      <FormErrors error={error} form={form} />
      <form ref={form} onSubmit={submit}>
      <fieldset disabled={busy} style={{ border: 0, margin: 0, padding: 0, minWidth: 0 }}>
        <CasePicker label="Other case" value={other} onChange={setOther} required />
        <SelectField
          label="Kind of relation"
          value={kind}
          onChange={setKind}
          options={options(refList(ref, 'relation_kind'))}
          placeholder="Choose the kind"
          required
          help="Links the cases without merging their histories."
        />
        <TextField label="Note (optional)" value={note} onChange={setNote} />
        <div className="actions">
          <Button type="submit" busy={busy} disabled={!other || !kind}>Add relation</Button>
          <Button type="button" variant="secondary" onClick={onClose}>Cancel</Button>
        </div>
      </fieldset>
      </form>
    </Modal>
  );
}

/* ------------------------------ the tab ------------------------------ */

export default function SummaryTab({ caseId, caseData, reload }: CaseTabProps) {
  const navigate = useNavigate();
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const c = caseData.case;
  const allowed = caseData.allowed;
  // `modal` keys remount each dialog so every open gets fresh state/keys.
  const [modal, setModal] = useState<'edit' | 'close' | 'assign' | 'relate' | 'hold' | 'reopen' | null>(null);
  const [modalTick, setModalTick] = useState(0);
  const [endAssignment, setEndAssignment] = useState<Assignment | null>(null);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<unknown>(null);
  const [residual, setResidual] = useState<{ name: string; roles: string[] } | null>(null);

  const openModal = (m: NonNullable<typeof modal>) => {
    setModalTick((t) => t + 1);
    setActionError(null);
    setModal(m);
  };

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

  const setStatus = (to: string, reason?: string) =>
    run(async () => {
      await api('POST', `/cases/${caseId}/status`, { to, reason: reason ?? null });
      setModal(null);
      reload();
    });

  const reopen = (reason: string) =>
    run(async () => {
      await api('POST', `/cases/${caseId}/reopen`, { reason });
      setModal(null);
      reload();
    });

  const endAssign = (reason: string) =>
    run(async () => {
      const res = await api<Partial<CaseData> & { residual_access: { role: string }[] }>('POST', `/cases/${caseId}/assignments/${endAssignment?.id}/end`, {
        reason,
      });
      const roles = (res?.residual_access ?? []).map((r) => r.role);
      const result = { name: endAssignment?.display_name ?? 'The person', roles };
      setResidual(result);
      setEndAssignment(null);
      if (!res.case) navigate('/cases', { state: { assignmentEnded: result } });
      else reload();
    });

  const activeAssignments = caseData.assignments.filter((a) => a.end_at === null);
  const endedAssignments = caseData.assignments.filter((a) => a.end_at !== null);

  const assignColumns = (withAction: boolean): Column<Assignment>[] => {
    const cols: Column<Assignment>[] = [
      { key: 'display_name', header: 'Person', render: (a) => `${a.display_name}${a.title ? ` — ${a.title}` : ''}` },
      { key: 'role', header: 'Role', render: (a) => ASSIGN_ROLE_LABELS[a.role] ?? a.role },
      { key: 'reason', header: 'Reason' },
      { key: 'start_at', header: 'From', render: (a) => fmtLocal(a.start_at) },
      {
        key: 'end_at',
        header: 'To',
        render: (a) => (a.end_at ? `${fmtLocal(a.end_at)}${a.end_reason ? ` — ${a.end_reason}` : ''}` : 'active'),
      },
      { key: 'assigned_by_name', header: 'By', render: (a) => a.assigned_by_name ?? '—' },
    ];
    if (withAction) {
      cols.push({
        key: 'id',
        header: '',
        render: (a) =>
          (a.role === 'judge' && allowed.assign_judge) || (a.role !== 'judge' && allowed.assign_staff) ? (
            <Button variant="secondary" onClick={() => { setActionError(null); setEndAssignment(a); }}>
              End assignment
            </Button>
          ) : null,
      });
    }
    return cols;
  };

  const canSetActive = allowed.set_status && ['registered', 'on_hold', 'reopened'].includes(c.status);
  const canSetHold = allowed.set_status && ['registered', 'active', 'reopened'].includes(c.status);

  return (
    <>
      <ErrorBanner error={refError} onRetry={reloadRef} />
      <Card
        title="Case details"
        actions={allowed.edit ? <Button variant="secondary" onClick={() => openModal('edit')}>Edit</Button> : undefined}
      >
        <div className="table-wrap">
          <table>
            <tbody>
              <Detail term="Number">{c.number}{c.legacy_number ? ` (was ${c.legacy_number})` : ''}</Detail>
              <Detail term="Title">{c.title}</Detail>
              <Detail term="Register">{c.series} — {c.registry_name}</Detail>
              <Detail term="Category">{c.category_label}</Detail>
              <Detail term="Status"><StatusBadge status={c.status} /></Detail>
              <Detail term="Restricted">
                {c.restricted ? 'Yes — hidden from staff without the right to see it' : 'No'}
              </Detail>
              <Detail term="Registered">
                {fmtDate(c.registered_date)}
                {c.registered_by_name ? ` by ${c.registered_by_name}` : ''}
              </Detail>
              <Detail term="Responsible">{c.responsible_name ?? '—'}</Detail>
              {c.summary && <Detail term="Summary">{c.summary}</Detail>}
              {Boolean(c.historical_incomplete) && (
                <Detail term="History">Imported with missing values — incomplete history.</Detail>
              )}
              {c.status === 'closed' && (
                <>
                  <Detail term="Closed">
                    {c.closed_date ? fmtDate(c.closed_date) : '—'}
                    {c.closed_by_name ? ` by ${c.closed_by_name}` : ''}
                  </Detail>
                  <Detail term="Basis">
                    {label(refList(ref, 'closure_basis'), c.closure_basis)}
                    {c.closure_note ? ` — ${c.closure_note}` : ''}
                  </Detail>
                </>
              )}
            </tbody>
          </table>
        </div>
      </Card>

      {(canSetActive || canSetHold || allowed.close || allowed.reopen) && (
        <Card title="Status">
          <div className="page-actions">
            {canSetActive && (
              <Button variant="secondary" busy={busy} onClick={() => void setStatus('active')}>Set active</Button>
            )}
            {canSetHold && (
              <Button variant="secondary" onClick={() => openModal('hold')}>Put on hold</Button>
            )}
            {allowed.reopen && (
              <Button variant="secondary" onClick={() => openModal('reopen')}>Reopen</Button>
            )}
            {allowed.close && (
              <Button variant="danger" onClick={() => openModal('close')}>Close the case</Button>
            )}
          </div>
        </Card>
      )}

      <Card
        title="Assignments"
        actions={
          allowed.assign_staff || allowed.assign_judge ? (
            <Button variant="secondary" onClick={() => openModal('assign')}>Assign</Button>
          ) : undefined
        }
      >
          {residual && (
            <div className="banner" role="status" style={{ background: '#eef4fd', border: '1px solid #c8d9f3' }}>
              <p>
                <strong>{residual.name}</strong> is no longer assigned in that role.
                {residual.roles.length > 0
                  ? ` They still have access as: ${residual.roles.map((r) => ASSIGN_ROLE_LABELS[r] ?? r).join(', ')}.`
                  : ' No access to this case remains through assignments.'}
              </p>
            </div>
          )}
        <ErrorBanner error={actionError} />
        <DataTable
          columns={assignColumns(true)}
          rows={activeAssignments}
          rowKey={(a) => String(a.id)}
          empty="Nobody is assigned to this case."
        />
        {endedAssignments.length > 0 && (
          <>
            <h3>Past assignments</h3>
            <DataTable
              columns={assignColumns(false)}
              rows={endedAssignments}
              rowKey={(a) => String(a.id)}
              empty=""
            />
          </>
        )}
      </Card>

      <Card
        title="Related cases"
        actions={allowed.edit ? <Button variant="secondary" onClick={() => openModal('relate')}>Add relation</Button> : undefined}
      >
        {caseData.relations.length === 0 ? (
          <p className="muted">No related cases.</p>
        ) : (
          <ul>
            {caseData.relations.map((r) => (
              <li key={`${r.direction}-${r.id}`}>
                <Link to={`/cases/${r.other_case_id}`}>{r.other_number}</Link> — {r.other_title}{' '}
                <span className="muted">
                  ({label(refList(ref, 'relation_kind'), r.kind)}
                  {r.direction === 'incoming' ? ', incoming' : ''}
                  {r.note ? ` — ${r.note}` : ''})
                </span>
              </li>
            ))}
          </ul>
        )}
      </Card>

      <Card title="Status history">
        <ul>
          {caseData.status_history.map((h, i) => (
            <li key={i}>
              <StatusBadge status={h.to_status} />{' '}
              <span className="muted">
                {h.effective_date ? fmtDate(h.effective_date) : fmtLocal(h.at)}
                {h.by_name ? ` — ${h.by_name}` : ''}
              </span>
              {h.basis && <> — {label(refList(ref, 'closure_basis'), h.basis)}</>}
              {h.reason && <div className="muted">{h.reason}</div>}
            </li>
          ))}
        </ul>
      </Card>

      <Card title="Linked filings">
        {caseData.intakes.length === 0 ? (
          <p className="muted">No filings linked to this case.</p>
        ) : (
          <ul>
            {caseData.intakes.map((i) => (
              <li key={i.id}>
                <Link to={`/intakes/${i.id}`}>{i.reference}</Link> — {i.sender_name},{' '}
                {fmtDate(i.received_date)}
                {i.parent_intake_id ? ' (supplement)' : ''}
                <div className="muted">{i.description}</div>
              </li>
            ))}
          </ul>
        )}
      </Card>

      {modal === 'edit' && (
        <EditCaseModal key={modalTick} caseData={caseData} onClose={() => setModal(null)} onSaved={reload} />
      )}
      {modal === 'close' && (
        <CloseCaseModal key={modalTick} caseId={caseId} onClose={() => setModal(null)} onSaved={reload} />
      )}
      {modal === 'assign' && (
        <AssignModal
          key={modalTick}
          caseId={caseId}
          allowed={allowed}
          staff={ref?.staff ?? []}
          onClose={() => setModal(null)}
          onSaved={reload}
        />
      )}
      {modal === 'relate' && (
        <AddRelationModal key={modalTick} caseId={caseId} onClose={() => setModal(null)} onSaved={reload} />
      )}
      <ReasonModal
        key={`hold-${modalTick}`}
        open={modal === 'hold'}
        title="Put the case on hold"
        label="Reason"
        confirmLabel="Put on hold"
        busy={busy}
        error={actionError}
        onConfirm={(r) => void setStatus('on_hold', r)}
        onClose={() => setModal(null)}
      />
      <ReasonModal
        key={`reopen-${modalTick}`}
        open={modal === 'reopen'}
        title="Reopen the case"
        label="Reason"
        confirmLabel="Reopen"
        busy={busy}
        error={actionError}
        onConfirm={(r) => void reopen(r)}
        onClose={() => setModal(null)}
      />
      {endAssignment && <ReasonModal
        key={endAssignment.id}
        open
        title={`End the assignment of ${endAssignment?.display_name ?? ''}`}
        label="Reason"
        confirmLabel="End assignment"
        busy={busy}
        error={actionError}
        onConfirm={(r) => void endAssign(r)}
        onClose={() => { if (!busy) setEndAssignment(null); }}
      />}
    </>
  );
}
