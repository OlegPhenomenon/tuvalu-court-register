/**
 * "Register as a new case" form (POST /intakes/:id/register → RegisterReq).
 * One Idempotency-Key per opened form (created by the parent with newKey()) so a
 * retry after a network failure can never mint a second case number.
 */

import { useRef, useState } from 'react';
import type { FormEvent } from 'react';
import { Button } from '../../components/Button';
import { ErrorBanner } from '../../components/ErrorBanner';
import { FormErrors } from './FormErrors';
import { CheckboxField, DateField, SelectField, TextArea, TextField } from '../../components/fields';
import { options, refList, staffOptions, useRef as useRefData } from '../../components/refdata';
import { courtToday } from '../../time';
import { useSession } from '../../session';
import { CasePicker, PartyPicker } from './pickers';
import type { CaseHit, Party } from './pickers';

export interface NewPartyValues {
  kind: 'person' | 'organisation';
  name: string;
  contact_email: string;
  contact_phone: string;
  address: string;
  island: string;
}

export interface ParticipantRow {
  key: number;
  mode: 'existing' | 'new';
  party: Party | null;
  newParty: NewPartyValues;
  role: string;
  service_contact: string;
}

export interface RegisterValues {
  registry_id: number | null;
  category: string;
  title: string;
  summary: string;
  registered_date: string;
  restricted: boolean;
  responsible_user_id: number | null;
  assignment_reason: string;
  related_case_id: number | null;
  participants: ParticipantRow[];
}

const EMPTY_PARTY: NewPartyValues = {
  kind: 'person',
  name: '',
  contact_email: '',
  contact_phone: '',
  address: '',
  island: '',
};

function blankRow(key: number, role = ''): ParticipantRow {
  return { key, mode: 'existing', party: null, newParty: { ...EMPTY_PARTY }, role, service_contact: '' };
}

/** Build the POST body; returns an error message string when a row is incomplete. */
export function registerPayload(v: RegisterValues): { body?: Record<string, unknown>; error?: string } {
  if (!v.registry_id) return { error: 'Choose the register (number series).' };
  if (!v.title.trim()) return { error: 'Title is required.' };
  if (!v.category) return { error: 'Choose a category.' };
  if (v.responsible_user_id && !v.assignment_reason.trim()) return { error: 'Give a reason for assigning another responsible officer.' };
  const participants = [];
  for (const [i, row] of v.participants.entries()) {
    if (!row.role) return { error: `Participant ${i + 1}: choose a role.` };
    if (row.mode === 'existing') {
      if (!row.party) return { error: `Participant ${i + 1}: pick an existing party or switch to "new".` };
      participants.push({ party_id: row.party.id, role: row.role, service_contact: row.service_contact || null });
    } else {
      if (!row.newParty.name.trim()) return { error: `Participant ${i + 1}: name is required.` };
      participants.push({
        new_party: {
          kind: row.newParty.kind,
          name: row.newParty.name,
          contact_email: row.newParty.contact_email || null,
          contact_phone: row.newParty.contact_phone || null,
          address: row.newParty.address || null,
          island: row.newParty.island || null,
        },
        role: row.role,
        service_contact: row.service_contact || null,
      });
    }
  }
  return {
    body: {
      registry_id: v.registry_id,
      category: v.category,
      title: v.title,
      summary: v.summary || null,
      registered_date: v.registered_date || null,
      restricted: v.restricted,
      responsible_user_id: v.responsible_user_id,
      assignment_reason: v.assignment_reason || null,
      related_case_id: v.related_case_id,
      participants,
    },
  };
}

function ParticipantEditor({
  row,
  roles,
  islands,
  onChange,
  onRemove,
}: {
  row: ParticipantRow;
  roles: { value: string; label: string }[];
  islands: { value: string; label: string }[];
  onChange: (row: ParticipantRow) => void;
  onRemove: () => void;
}) {
  const set = (patch: Partial<ParticipantRow>) => onChange({ ...row, ...patch });
  const setParty = (patch: Partial<NewPartyValues>) =>
    onChange({ ...row, newParty: { ...row.newParty, ...patch } });
  return (
    <fieldset
      style={{ minWidth: 0, border: '1px solid var(--line)', borderRadius: '8px', margin: '0 0 1rem', padding: '0.75rem' }}
    >
      <legend style={{ fontWeight: 600, padding: '0 0.4rem' }}>Participant</legend>
      <SelectField
        label="Role in this case"
        value={row.role}
        onChange={(role) => set({ role })}
        options={roles}
        placeholder="Choose a role"
        required
      />
      <SelectField
        label="Who is it?"
        value={row.mode}
        onChange={(mode) => set({ mode: mode as ParticipantRow['mode'] })}
        options={[
          { value: 'existing', label: 'An existing party record' },
          { value: 'new', label: 'A new person or organisation' },
        ]}
      />
      {row.mode === 'existing' ? (
        <PartyPicker value={row.party} onChange={(party) => set({ party })} required help="A name match never merges records — pick the record itself." />
      ) : (
        <>
          <SelectField
            label="Kind"
            value={row.newParty.kind}
            onChange={(kind) => setParty({ kind: kind as NewPartyValues['kind'] })}
            options={[
              { value: 'person', label: 'Person' },
              { value: 'organisation', label: 'Organisation' },
            ]}
            required
          />
          <TextField label="Full name" value={row.newParty.name} onChange={(name) => setParty({ name })} required />
          <TextField label="E-mail" type="email" value={row.newParty.contact_email} onChange={(contact_email) => setParty({ contact_email })} />
          <TextField label="Phone" value={row.newParty.contact_phone} onChange={(contact_phone) => setParty({ contact_phone })} />
          <TextField label="Address" value={row.newParty.address} onChange={(address) => setParty({ address })} />
          <SelectField
            label="Island"
            value={row.newParty.island}
            onChange={(island) => setParty({ island })}
            options={islands}
            placeholder="Not recorded"
          />
        </>
      )}
      <TextField
        label="Service contact for this case"
        value={row.service_contact}
        onChange={(service_contact) => set({ service_contact })}
        help="Address or e-mail used to send documents in this case."
      />
      <div className="actions">
        <Button type="button" variant="secondary" onClick={onRemove}>
          Remove participant
        </Button>
      </div>
    </fieldset>
  );
}

export function RegisterForm({
  senderName,
  senderPartyId,
  busy,
  error,
  onSubmit,
  onCancel,
}: {
  senderName: string;
  senderPartyId: number | null;
  busy?: boolean;
  error?: unknown;
  onSubmit: (values: RegisterValues) => void;
  onCancel: () => void;
}) {
  const { hasPerm, session } = useSession();
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const form = useRef<HTMLFormElement>(null);
  const roles = refList(ref, 'participant_role');
  const senderRole = roles.some((r) => r.code === 'claimant') ? 'claimant' : (roles.some((r) => r.code === 'applicant') ? 'applicant' : 'claimant');
  const [v, setV] = useState<RegisterValues>(() => ({
    registry_id: null,
    category: '',
    title: '',
    summary: '',
    registered_date: courtToday(),
    restricted: false,
    responsible_user_id: null,
    assignment_reason: '',
    related_case_id: null,
    // Spec: one row pre-filled with the sender as claimant/applicant.
    participants: [
      {
        ...blankRow(0, senderRole),
        mode: senderPartyId ? 'existing' : 'new',
        party: senderPartyId ? { id: senderPartyId, kind: 'party record', name: senderName } : null,
        newParty: { ...EMPTY_PARTY, name: senderName },
      },
    ],
  }));
  const [related, setRelated] = useState<CaseHit | null>(null);
  const [localError, setLocalError] = useState('');
  const [rowKey, setRowKey] = useState(1);

  const setRow = (key: number, row: ParticipantRow) =>
    setV((prev) => ({ ...prev, participants: prev.participants.map((r) => (r.key === key ? row : r)) }));

  const submit = (e: FormEvent) => {
    e.preventDefault();
    const values = { ...v, responsible_user_id: hasPerm('case.assign_staff') ? v.responsible_user_id : null, related_case_id: related?.id ?? null };
    const { error: msg } = registerPayload(values);
    if (msg) {
      setLocalError(msg);
      return;
    }
    setLocalError('');
    onSubmit(values);
  };

  return (
    <>
      <FormErrors error={error} form={form} />
      <ErrorBanner error={refError} onRetry={reloadRef} />
      <form ref={form} onSubmit={submit}>
      <fieldset disabled={busy} style={{ border: 0, padding: 0, margin: 0, minWidth: 0 }}>
      {localError && (
        <div className="banner banner--error" role="alert">
          <p>{localError}</p>
        </div>
      )}
      <SelectField
        label="Register (number series)"
        value={v.registry_id ? String(v.registry_id) : ''}
        onChange={(s) => setV((prev) => ({ ...prev, registry_id: s ? Number(s) : null }))}
        options={(ref?.registries ?? []).map((r) => ({ value: String(r.id), label: `${r.series} — ${r.name}` }))}
        placeholder="Choose the register"
        required
      />
      <SelectField
        label="Category"
        value={v.category}
        onChange={(category) => setV((prev) => ({ ...prev, category }))}
        options={options(refList(ref, 'case_category'))}
        placeholder="Choose a category"
        required
      />
      <TextField
        label="Title"
        value={v.title}
        onChange={(title) => setV((prev) => ({ ...prev, title }))}
        required
        help="Short plain-language name, e.g. the parties' names."
      />
      <TextArea
        label="Summary"
        value={v.summary}
        onChange={(summary) => setV((prev) => ({ ...prev, summary }))}
        rows={3}
      />
      <DateField
        label="Registration date"
        max={courtToday()}
        value={v.registered_date}
        onChange={(registered_date) => setV((prev) => ({ ...prev, registered_date }))}
        required
      />
      <CheckboxField
        label="Restricted case"
        checked={v.restricted}
        onChange={(restricted) => setV((prev) => ({ ...prev, restricted }))}
        help="A restricted case does not appear in general lists, search suggestions or counts for staff without the right to see it. Restrict access only when the case requires it."
      />
      {hasPerm('case.assign_staff') && <>
        <SelectField
          label="Responsible officer"
          value={v.responsible_user_id ? String(v.responsible_user_id) : ''}
          onChange={(s) => setV((prev) => ({ ...prev, responsible_user_id: s ? Number(s) : null }))}
          options={staffOptions((ref?.staff ?? []).filter((s) => s.assignable !== false && !s.is_judge && s.id !== Number(session.user.id)))}
          placeholder="You (the registering clerk)"
        />
        {v.responsible_user_id && <TextArea label="Reason for assigning responsible officer" value={v.assignment_reason}
          onChange={(assignment_reason) => setV(prev => ({...prev, assignment_reason}))} required rows={2} />}
      </>}
      <CasePicker
        label="Earlier related case (optional)"
        value={related}
        onChange={setRelated}
        help="Record only when this filing is a follow-up application to a case already in the register."
      />

      <h3>Participants</h3>
      <p className="muted">Who takes part in the case and in which role.</p>
      {v.participants.map((row) => (
        <ParticipantEditor
          key={row.key}
          row={row}
          roles={options(roles)}
          islands={options(refList(ref, 'origin_island'))}
          onChange={(r) => setRow(row.key, r)}
          onRemove={() =>
            setV((prev) => ({ ...prev, participants: prev.participants.filter((r) => r.key !== row.key) }))
          }
        />
      ))}
      <p>
        <Button
          type="button"
          variant="secondary"
          onClick={() => {
            setRowKey((k) => k + 1);
            setV((prev) => ({ ...prev, participants: [...prev.participants, blankRow(rowKey)] }));
          }}
        >
          Add another participant
        </Button>
      </p>

      <div className="actions">
        <Button type="submit" busy={busy}>
          Register the case
        </Button>
        <Button type="button" variant="secondary" onClick={onCancel}>
          Cancel
        </Button>
      </div>
      </fieldset>
    </form>
    </>
  );
}
