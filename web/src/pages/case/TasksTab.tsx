/**
 * Tasks tab (C10): open tasks first, then closed ones. Tasks track work — they
 * never decide the outcome of the case. Completing needs a result; cancelling
 * and carrying forward need a reason; editing uses optimistic locking.
 * State changes are open to task.manage holders and to the task's assignee.
 */

import { useRef, useState } from 'react';
import type { FormEvent } from 'react';
import { Link } from 'react-router-dom';
import { api, ApiError } from '../../api';
import { useSession } from '../../session';
import { fmtCourtLocal, fmtDate, fmtLocal } from '../../time';
import { Button } from '../../components/Button';
import { Card } from '../../components/Card';
import { DataTable } from '../../components/DataTable';
import type { Column } from '../../components/DataTable';
import { ErrorBanner } from '../../components/ErrorBanner';
import { DateField, SelectField, TextArea, TextField } from '../../components/fields';
import { Modal } from '../../components/Modal';
import { StatusBadge } from '../../components/StatusBadge';
import { useApi } from '../../components/useApi';
import { caseStaffOptions } from '../../components/HearingForm';
import type { Hearing } from '../../components/HearingForm';
import type { CaseTabProps } from './types';

/** Mirrors task_json in src/api/tasks.rs. */
interface Task {
  id: number;
  case_id: number | null;
  case_number: string | null;
  intake_id: number | null;
  hearing_id: number | null;
  kind: string;
  title: string;
  description: string | null;
  assignee_user_id: number | null;
  assignee_name: string | null;
  due_date: string | null;
  status: string;
  result: string | null;
  status_reason: string | null;
  created_by_name: string | null;
  created_at: string;
  closed_by_name: string | null;
  closed_at: string | null;
  version: number;
}

const KIND_LABELS: Record<string, string> = {
  renotify: 'Re-notification',
  follow_up: 'Follow-up',
};

/** Single required-text dialog (result, reason) with an inline error slot. */
function PromptModal({ title, label: fieldLabel, confirmLabel = 'Confirm', danger, busy, error, onConfirm, onClose }: {
  title: string;
  label: string;
  confirmLabel?: string;
  danger?: boolean;
  busy?: boolean;
  error?: unknown;
  onConfirm: (text: string) => void;
  onClose: () => void;
}) {
  const [text, setText] = useState('');
  return (
    <Modal title={title} open onClose={busy ? () => {} : onClose}>
      <ErrorBanner error={error} onRetry={text.trim() ? () => onConfirm(text.trim()) : undefined} />
      <TextArea disabled={busy} label={fieldLabel} value={text} onChange={setText} required rows={3} autoFocus />
      <div className="actions">
        <Button
          variant={danger ? 'danger' : 'primary'}
          busy={busy}
          disabled={!text.trim()}
          onClick={() => onConfirm(text.trim())}
        >
          {confirmLabel}
        </Button>
        <Button variant="secondary" disabled={busy} onClick={onClose}>Back</Button>
      </div>
    </Modal>
  );
}

/** Create / edit form; editing sends `version` and keeps the hearing link fixed. */
function TaskForm({ caseId, caseData, hearings, task, onClose, onSaved }: {
  caseId: number;
  caseData: CaseTabProps['caseData'];
  hearings: Hearing[];
  task?: Task;
  onClose: () => void;
  onSaved: () => void;
}) {
  const { session } = useSession();
  const form = useRef<HTMLFormElement>(null);
  const editing = Boolean(task);
  const [version, setVersion] = useState(task?.version ?? 0);
  const [title, setTitle] = useState(task?.title ?? '');
  const [description, setDescription] = useState(task?.description ?? '');
  const [assignee, setAssignee] = useState(task?.assignee_user_id ? String(task.assignee_user_id) : '');
  const [due, setDue] = useState(task?.due_date ?? '');
  const [hearingId, setHearingId] = useState(task?.hearing_id ? String(task.hearing_id) : '');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [attempted, setAttempted] = useState<Record<string, unknown> | undefined>();

  const assignees = caseStaffOptions(caseData, session.user);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    const body = editing
      ? {
          version,
          title,
          description,
          assignee_user_id: assignee ? Number(assignee) : null,
          due_date: due,
        }
      : {
          title,
          description: description || null,
          assignee_user_id: assignee ? Number(assignee) : null,
          due_date: due || null,
          hearing_id: hearingId ? Number(hearingId) : null,
        };
    setAttempted(body as Record<string, unknown>);
    setBusy(true);
    setError(null);
    try {
      if (editing) await api('PATCH', `/tasks/${task!.id}`, body);
      else await api('POST', `/cases/${caseId}/tasks`, body);
      onSaved();
      onClose();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  const versionConflict = error instanceof ApiError && error.code === 'version_conflict';
  const currentVersion = versionConflict
    ? (error.details as { current?: { version?: number } })?.current?.version
    : undefined;

  return (
    <Modal title={editing ? `Edit task: ${task!.title}` : 'Add a task'} open onClose={busy ? () => {} : onClose}>
      <ErrorBanner
        error={error}
        attempted={attempted}
        onRetry={error && !versionConflict ? () => form.current?.requestSubmit() : undefined}
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
      <form ref={form} onSubmit={submit}>
        <fieldset disabled={busy} style={{ border: 0, margin: 0, padding: 0, minWidth: 0 }}>
          <TextField label="Title" value={title} onChange={setTitle} required />
          <TextArea label="Description" value={description} onChange={setDescription} rows={3} />
          <SelectField
            label="Assign to"
            value={assignee}
            onChange={setAssignee}
            options={assignees}
            placeholder="Unassigned"
            help="Only staff with access to this case can take a task."
          />
          <DateField
            label="Due date (set by staff)"
            value={due}
            onChange={setDue}
            help="A working date entered by a person — the system never computes legal deadlines."
          />
          {!editing && hearings.length > 0 && (
            <SelectField
              label="Linked hearing (optional)"
              value={hearingId}
              onChange={setHearingId}
              options={hearings.map((h) => ({
                value: String(h.id),
                label: `${h.hearing_type_label} — ${h.starts_local ? fmtCourtLocal(h.starts_local) : fmtLocal(h.starts_at)}`,
              }))}
              placeholder="Not linked to a hearing"
            />
          )}
          <div className="actions">
            <Button type="submit" busy={busy} disabled={!title.trim()}>
              {editing ? 'Save changes' : 'Add task'}
            </Button>
            <Button type="button" variant="secondary" onClick={onClose}>Cancel</Button>
          </div>
        </fieldset>
      </form>
    </Modal>
  );
}

type ModalState =
  | { kind: 'create' }
  | { kind: 'edit'; task: Task }
  | { kind: 'complete'; task: Task }
  | { kind: 'cancel'; task: Task }
  | { kind: 'carry'; task: Task }
  | null;

export default function TasksTab({ caseId, caseData, reload }: CaseTabProps) {
  const { session } = useSession();
  const { data, error, loading, reload: reloadList } = useApi<{ items: Task[] }>(
    `/cases/${caseId}/tasks`,
  );
  const { data: hearingsData } = useApi<{ items: Hearing[] }>(`/cases/${caseId}/hearings`);
  const allowed = caseData.allowed;
  const [modal, setModal] = useState<ModalState>(null);
  const [modalTick, setModalTick] = useState(0);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState<unknown>(null);

  const items = data?.items ?? [];
  const hearings = hearingsData?.items ?? [];
  const hearingById = new Map(hearings.map((h) => [h.id, h]));
  const meId = Number(session.user.id);
  // Mirrors require_touch in tasks.rs: task.manage or being the assignee.
  const canTouch = (t: Task) => allowed.manage_tasks || t.assignee_user_id === meId;

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

  const act = async (path: string, body: Record<string, unknown>) => {
    setBusy(true);
    setActionError(null);
    try {
      await api('POST', path, body);
      finish();
    } catch (e) {
      setActionError(e);
    } finally {
      setBusy(false);
    }
  };

  const columns: Column<Task>[] = [
    {
      key: 'title',
      header: 'Task',
      render: (t) => (
        <>
          <strong>{t.title}</strong>
          {t.kind !== 'general' && KIND_LABELS[t.kind] && (
            <span className="muted"> — {KIND_LABELS[t.kind]}</span>
          )}
          {t.description && <div className="muted">{t.description}</div>}
        </>
      ),
    },
    { key: 'assignee_name', header: 'Assigned to', render: (t) => t.assignee_name ?? '—' },
    {
      key: 'due_date',
      header: 'Due (set by staff)',
      render: (t) => (t.due_date ? fmtDate(t.due_date) : '—'),
    },
    { key: 'status', header: 'Status', render: (t) => <StatusBadge status={t.status} /> },
    {
      key: 'result',
      header: 'Result / reason',
      render: (t) => (
        <>
          {t.result ?? t.status_reason ?? '—'}
          {t.closed_by_name && <div className="muted">by {t.closed_by_name}</div>}
        </>
      ),
    },
    {
      key: 'hearing_id',
      header: 'Hearing',
      render: (t) => {
        const h = t.hearing_id ? hearingById.get(t.hearing_id) : undefined;
        if (!h) return '—';
        return (
          <Link to={`?tab=hearings&hearing=${h.id}`}>
            {h.hearing_type_label} — {h.starts_local ? fmtCourtLocal(h.starts_local) : fmtLocal(h.starts_at)}
          </Link>
        );
      },
    },
    {
      key: 'id',
      header: '',
      render: (t) =>
        t.status === 'open' && canTouch(t) ? (
          <div className="page-actions">
            <Button variant="secondary" onClick={() => openModal({ kind: 'complete', task: t })}>Complete</Button>
            {allowed.manage_tasks && (
              <Button variant="secondary" onClick={() => openModal({ kind: 'edit', task: t })}>Edit</Button>
            )}
            <Button variant="secondary" onClick={() => openModal({ kind: 'carry', task: t })}>Carry forward</Button>
            <Button variant="secondary" onClick={() => openModal({ kind: 'cancel', task: t })}>Cancel</Button>
          </div>
        ) : null,
    },
  ];

  return (
    <>
      <Card
        title="Tasks"
        actions={
          allowed.manage_tasks ? (
            <Button onClick={() => openModal({ kind: 'create' })}>Add task</Button>
          ) : undefined
        }
      >
        <p className="muted">
          Tasks track work; they never decide the outcome of the case. Open tasks must be completed,
          cancelled with a reason, or carried forward before the case can be closed.
        </p>
        <ErrorBanner error={error} onRetry={reloadList} />
        {loading && !data ? (
          <p className="muted">Loading…</p>
        ) : (
          <DataTable
            columns={columns}
            rows={items}
            rowKey={(t) => String(t.id)}
            empty="No tasks on this case."
          />
        )}
      </Card>

      {(modal?.kind === 'create' || modal?.kind === 'edit') && (
        <TaskForm
          key={modalTick}
          caseId={caseId}
          caseData={caseData}
          hearings={hearings}
          task={modal.kind === 'edit' ? modal.task : undefined}
          onClose={() => setModal(null)}
          onSaved={finish}
        />
      )}
      {modal?.kind === 'complete' && (
        <PromptModal
          key={modalTick}
          title={`Complete: ${modal.task.title}`}
          label="Result"
          confirmLabel="Mark done"
          busy={busy}
          error={actionError}
          onConfirm={(r) => void act(`/tasks/${modal.task.id}/complete`, { result: r })}
          onClose={() => setModal(null)}
        />
      )}
      {modal?.kind === 'cancel' && (
        <PromptModal
          key={modalTick}
          title={`Cancel: ${modal.task.title}`}
          label="Reason for cancelling"
          confirmLabel="Cancel the task"
          danger
          busy={busy}
          error={actionError}
          onConfirm={(r) => void act(`/tasks/${modal.task.id}/cancel`, { reason: r })}
          onClose={() => setModal(null)}
        />
      )}
      {modal?.kind === 'carry' && (
        <PromptModal
          key={modalTick}
          title={`Carry forward: ${modal.task.title}`}
          label="Why is it left for later work?"
          confirmLabel="Carry forward"
          busy={busy}
          error={actionError}
          onConfirm={(r) => void act(`/tasks/${modal.task.id}/carry-forward`, { reason: r })}
          onClose={() => setModal(null)}
        />
      )}
    </>
  );
}
