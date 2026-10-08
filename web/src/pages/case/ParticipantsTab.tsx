/**
 * Participants tab (C04): who takes part in the case, in which role, with a
 * representative and a service contact. A name match never merges party
 * records — a new party is created through POST /api/parties, whose
 * `same_name_records` are shown as a warning, then linked to the case.
 */

import { useRef, useState } from 'react';
import type { FormEvent } from 'react';
import { api } from '../../api';
import { Button } from '../../components/Button';
import { Card } from '../../components/Card';
import { DataTable } from '../../components/DataTable';
import type { Column } from '../../components/DataTable';
import { ErrorBanner } from '../../components/ErrorBanner';
import { SelectField, TextArea, TextField } from '../../components/fields';
import { Modal } from '../../components/Modal';
import { label, options, refList, useRef as useRefData } from '../../components/refdata';
import { fmtLocal } from '../../time';
import { FormErrors } from '../intake/FormErrors';
import { PartyPicker } from '../intake/pickers';
import type { Party } from '../intake/pickers';
import type { CaseTabProps, Participant } from './types';

interface SameNameRecord {
  id: number;
  kind: string;
  name: string;
  contact_email: string | null;
  island: string | null;
}

function AddParticipantModal({ caseId, onClose, onSaved, onSameName }: {
  caseId: number;
  onClose: () => void;
  onSaved: () => void;
  onSameName: (records: SameNameRecord[], name: string) => void;
}) {
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const form = useRef<HTMLFormElement>(null);
  const [sameName, setSameName] = useState<SameNameRecord[]>([]);
  const [mode, setMode] = useState<'existing' | 'new'>('existing');
  const [party, setParty] = useState<Party | null>(null);
  const [kind, setKind] = useState<'person' | 'organisation'>('person');
  const [name, setName] = useState('');
  const [email, setEmail] = useState('');
  const [phone, setPhone] = useState('');
  const [address, setAddress] = useState('');
  const [island, setIsland] = useState('');
  const [role, setRole] = useState('');
  const [rep, setRep] = useState<Party | null>(null);
  const [repBasis, setRepBasis] = useState('');
  const [serviceContact, setServiceContact] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [localError, setLocalError] = useState('');

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setLocalError('');
    if (!role) {
      setLocalError('Choose the role in this case.');
      return;
    }
    if (mode === 'existing' && !party) {
      setLocalError('Pick an existing party record, or switch to "new".');
      return;
    }
    if (mode === 'new' && !name.trim()) {
      setLocalError('Name is required.');
      return;
    }
    if (rep && !repBasis.trim()) {
      setLocalError('State the basis of representation.');
      return;
    }
    setBusy(true);
    setError(null);
    try {
      let partyId = party?.id ?? null;
      if (mode === 'new') {
        const created = await api<{ id: number; same_name_records: SameNameRecord[] }>(
          'POST',
          '/parties',
          {
            kind,
            name,
            contact_email: email || null,
            contact_phone: phone || null,
            address: address || null,
            island: island || null,
          },
        );
        partyId = created.id;
        // Reuse the successfully created record if adding the participation fails.
        setParty({ id: created.id, kind, name, contact_email: email || null });
        setMode('existing');
        setSameName(created.same_name_records);
        if (created.same_name_records.length > 0) onSameName(created.same_name_records, name.trim());
      }
      await api('POST', `/cases/${caseId}/participants`, {
        party_id: partyId,
        role,
        representative_party_id: rep?.id ?? null,
        representation_basis: rep ? repBasis : null,
        service_contact: serviceContact || null,
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
    <Modal title="Add a participant" open onClose={busy ? () => {} : onClose}>
      <FormErrors error={error} form={form} />
      <ErrorBanner error={refError} onRetry={reloadRef} />
      {sameName.length > 0 && <p role="alert" className="badge badge--warn" style={{ whiteSpace: 'normal' }}>
        Other records with the same name exist; they were NOT merged.
      </p>}
      <form ref={form} onSubmit={submit}>
      <fieldset disabled={busy} style={{ border: 0, margin: 0, padding: 0, minWidth: 0 }}>
        {localError && (
          <div className="banner banner--error" role="alert">
            <p>{localError}</p>
          </div>
        )}
        <SelectField
          label="Role in this case"
          value={role}
          onChange={setRole}
          options={options(refList(ref, 'participant_role'))}
          placeholder="Choose a role"
          required
        />
        <SelectField
          label="Who is it?"
          value={mode}
          onChange={(m) => setMode(m as 'existing' | 'new')}
          options={[
            { value: 'existing', label: 'An existing party record' },
            { value: 'new', label: 'A new person or organisation' },
          ]}
        />
        {mode === 'existing' ? (
          <PartyPicker value={party} onChange={setParty} required />
        ) : (
          <>
            <SelectField
              label="Kind"
              value={kind}
              onChange={(k) => setKind(k as 'person' | 'organisation')}
              options={[
                { value: 'person', label: 'Person' },
                { value: 'organisation', label: 'Organisation' },
              ]}
              required
            />
            <TextField label="Full name" value={name} onChange={setName} required />
            <TextField label="E-mail" type="email" value={email} onChange={setEmail} />
            <TextField label="Phone" value={phone} onChange={setPhone} />
            <TextField label="Address" value={address} onChange={setAddress} />
            <SelectField
              label="Island"
              value={island}
              onChange={setIsland}
              options={options(refList(ref, 'origin_island'))}
              placeholder="Not recorded"
            />
          </>
        )}
        <PartyPicker
          label="Representative (optional)"
          value={rep}
          onChange={setRep}
          help="The person or organisation acting for this participant."
        />
        {rep && (
          <TextField
            label="Basis of representation"
            value={repBasis}
            onChange={setRepBasis}
            required
            help="e.g. power of attorney, court appointment."
          />
        )}
        <TextField
          label="Service contact for this case"
          value={serviceContact}
          onChange={setServiceContact}
          help="Address or e-mail used to send documents in this case."
        />
        <div className="actions">
          <Button type="submit" busy={busy}>Add participant</Button>
          <Button type="button" variant="secondary" onClick={onClose}>Cancel</Button>
        </div>
      </fieldset>
      </form>
    </Modal>
  );
}

function EndParticipationModal({ participant, caseId, onClose, onSaved }: {
  participant: Participant;
  caseId: number;
  onClose: () => void;
  onSaved: () => void;
}) {
  const [reason, setReason] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const submit = async () => {
    setBusy(true);
    setError(null);
    try {
      await api('POST', `/cases/${caseId}/participants/${participant.id}/end`, { reason });
      onSaved();
      onClose();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <Modal title={`End participation of ${participant.name}`} open onClose={busy ? () => {} : onClose}>
      <ErrorBanner error={error} onRetry={() => void submit()} />
      <TextArea disabled={busy} label="Reason" value={reason} onChange={setReason} required rows={3} autoFocus />
      <div className="actions">
        <Button variant="danger" busy={busy} disabled={!reason.trim()} onClick={() => void submit()}>
          End participation
        </Button>
        <Button variant="secondary" disabled={busy} onClick={onClose}>Cancel</Button>
      </div>
    </Modal>
  );
}

export default function ParticipantsTab({ caseId, caseData, reload }: CaseTabProps) {
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const allowed = caseData.allowed;
  const [addOpen, setAddOpen] = useState(false);
  const [ending, setEnding] = useState<Participant | null>(null);
  const [sameName, setSameName] = useState<{ name: string; records: SameNameRecord[] } | null>(null);

  const columns = (active: boolean): Column<Participant>[] => {
    const cols: Column<Participant>[] = [
      {
        key: 'name',
        header: 'Name',
        render: (p) => (
          <>
            <strong>{p.name}</strong> <span className="muted">({p.kind})</span>
            {p.contact_email && <div className="muted">{p.contact_email}</div>}
            {p.contact_phone && <div className="muted">{p.contact_phone}</div>}
          </>
        ),
      },
      { key: 'role', header: 'Role', render: (p) => label(refList(ref, 'participant_role'), p.role) },
      {
        key: 'representative_party_id',
        header: 'Representative',
        render: (p) =>
          p.representative_name ? (
            <>
              {p.representative_name}
              {p.representation_basis && <div className="muted">basis: {p.representation_basis}</div>}
            </>
          ) : (
            '—'
          ),
      },
      { key: 'service_contact', header: 'Service contact', render: (p) => p.service_contact ?? '—' },
      { key: 'added_at', header: 'From', render: (p) => fmtLocal(p.added_at) },
    ];
    if (active) {
      cols.push({
        key: 'id',
        header: '',
        render: (p) =>
          allowed.edit ? (
            <Button variant="secondary" onClick={() => setEnding(p)}>End participation</Button>
          ) : null,
      });
    } else {
      cols.push({
        key: 'ended_at',
        header: 'Ended',
        render: (p) => (
          <>
            {p.ended_at ? fmtLocal(p.ended_at) : '—'}
            {p.end_reason && <div className="muted">{p.end_reason}</div>}
          </>
        ),
      });
    }
    return cols;
  };

  const active = caseData.participants.filter((p) => p.active);
  const ended = caseData.participants.filter((p) => !p.active);

  return (
    <>
      <ErrorBanner error={refError} onRetry={reloadRef} />
      <Card
        title="Participants"
        actions={
          allowed.edit ? (
            <Button variant="secondary" onClick={() => setAddOpen(true)}>Add participant</Button>
          ) : undefined
        }
      >
        {sameName && (
          <div
            className="banner"
            role="alert"
            style={{ background: '#fdeacc', border: '1px solid #ecc97e', color: '#5d3c00' }}
          >
            <p>
              <strong>Other records with the same name exist; they were NOT merged.</strong> Name: {sameName.name}.{' '}
              The new record is a separate party. If you picked the wrong one, end this participation
              and add the existing record instead.
            </p>
            <ul>
              {sameName.records.map((r) => (
                <li key={r.id}>
                  {r.name} ({r.kind}
                  {r.island ? `, ${r.island}` : ''}
                  {r.contact_email ? `, ${r.contact_email}` : ''})
                </li>
              ))}
            </ul>
            <div className="banner-actions">
              <Button variant="secondary" onClick={() => setSameName(null)}>Understood</Button>
            </div>
          </div>
        )}
        <DataTable
          columns={columns(true)}
          rows={active}
          rowKey={(p) => String(p.id)}
          empty="No active participants."
        />
        {ended.length > 0 && (
          <>
            <h3>Ended participations</h3>
            <DataTable columns={columns(false)} rows={ended} rowKey={(p) => String(p.id)} empty="" />
          </>
        )}
      </Card>

      {addOpen && (
        <AddParticipantModal
          caseId={caseId}
          onClose={() => setAddOpen(false)}
          onSaved={reload}
          onSameName={(records, name) => setSameName({ records, name })}
        />
      )}
      {ending && (
        <EndParticipationModal
          participant={ending}
          caseId={caseId}
          onClose={() => setEnding(null)}
          onSaved={reload}
        />
      )}
    </>
  );
}
