/**
 * The filing form, shared by "Receive filing" (POST /intakes), "Add supplement"
 * (POST /intakes/:id/supplement) and "Edit" (PATCH /intakes/:id). Fields map
 * one-to-one to the backend IntakeInput. Typed text stays in state on failure;
 * the parent shows ErrorBanner and retries with the same Idempotency-Key.
 */

import { useRef, useState } from 'react';
import type { FormEvent } from 'react';
import { Button } from '../../components/Button';
import { ErrorBanner } from '../../components/ErrorBanner';
import { FormErrors } from './FormErrors';
import { CheckboxField, DateField, SelectField, TextArea, TextField } from '../../components/fields';
import { options, refList, useRef as useRefData } from '../../components/refdata';
import { courtToday } from '../../time';
import { PartyPicker } from './pickers';
import type { Party } from './pickers';

export interface IntakeFormValues {
  sender_name: string;
  sender_party_id: number | null;
  channel: string;
  origin_island: string;
  document_date: string;
  received_date: string;
  description: string;
  is_paper_original: boolean;
  paper_location: string;
}

export const EMPTY_INTAKE: IntakeFormValues = {
  sender_name: '',
  sender_party_id: null,
  channel: '',
  origin_island: '',
  document_date: '',
  received_date: '',
  description: '',
  is_paper_original: false,
  paper_location: '',
};

/** Row of `intakes` (GET /api/intakes/:id → intake) → form values. */
export function intakeFormValues(intake: Record<string, unknown>): IntakeFormValues {
  const s = (k: string) => (typeof intake[k] === 'string' ? (intake[k] as string) : '');
  return {
    sender_name: s('sender_name'),
    sender_party_id: typeof intake.sender_party_id === 'number' ? intake.sender_party_id : null,
    channel: s('channel'),
    origin_island: s('origin_island'),
    document_date: s('document_date'),
    received_date: s('received_date') || courtToday(),
    description: s('description'),
    is_paper_original: Boolean(intake.is_paper_original),
    paper_location: s('paper_location'),
  };
}

/** Serialise to the backend IntakeInput shape (empty strings → null where optional). */
export function intakePayload(v: IntakeFormValues) {
  return {
    sender_name: v.sender_name,
    sender_party_id: v.sender_party_id,
    channel: v.channel,
    origin_island: v.origin_island || null,
    document_date: v.document_date || null,
    received_date: v.received_date,
    description: v.description,
    is_paper_original: v.is_paper_original,
    paper_location: v.is_paper_original ? v.paper_location || null : null,
  };
}

export function IntakeForm({
  initial,
  submitLabel,
  busy,
  error,
  attempted,
  onSubmit,
  onCancel,
  onReviewVersion,
}: {
  initial?: Partial<IntakeFormValues>;
  submitLabel: string;
  busy?: boolean;
  error?: unknown;
  attempted?: Record<string, unknown>;
  onSubmit: (values: IntakeFormValues) => void;
  onCancel?: () => void;
  onReviewVersion?: (version: number) => void;
}) {
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const form = useRef<HTMLFormElement>(null);
  const [v, setV] = useState<IntakeFormValues>(() => ({ ...EMPTY_INTAKE, received_date: courtToday(), ...initial }));
  const [senderParty, setSenderParty] = useState<Party | null>(() => initial?.sender_party_id ? { id: initial.sender_party_id, name: initial.sender_name ?? '', kind: 'party record' } : null);
  const [localError, setLocalError] = useState('');

  const set = <K extends keyof IntakeFormValues>(k: K, val: IntakeFormValues[K]) =>
    setV((prev) => ({ ...prev, [k]: val }));

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (v.is_paper_original && !v.paper_location.trim()) {
      setLocalError('Say where the paper original is kept.');
      return;
    }
    setLocalError('');
    onSubmit(v);
  };

  return (
    <>
      <FormErrors error={error} form={form} attempted={attempted} onReviewVersion={onReviewVersion} />
      <ErrorBanner error={refError} onRetry={reloadRef} />
      <form ref={form} onSubmit={submit}>
      <fieldset disabled={busy} style={{ border: 0, padding: 0, margin: 0, minWidth: 0 }}>
      {localError && (
        <div className="banner banner--error" role="alert">
          <p>{localError}</p>
        </div>
      )}
      <PartyPicker
        label="Existing party record (optional)"
        value={senderParty}
        onChange={(p) => {
          setSenderParty(p);
          if (p) {
            setV((prev) => ({ ...prev, sender_name: p.name, sender_party_id: p.id }));
          } else {
            setV((prev) => ({ ...prev, sender_party_id: null }));
          }
        }}
        help="Pick a known sender, or just type the name below. Picking a record links this filing to it."
      />
      <TextField
        label="Sender name"
        value={v.sender_name}
        onChange={(s) => {
          // Editing the name after picking a record means a different sender — drop the link.
          // (A party link loaded from the record itself is kept unless the picker changes it.)
          setV((prev) => ({
            ...prev,
            sender_name: s,
            sender_party_id: senderParty && s !== senderParty.name ? null : prev.sender_party_id,
          }));
          if (senderParty && s !== senderParty.name) setSenderParty(null);
        }}
        required
      />
      <SelectField
        label="Channel"
        value={v.channel}
        onChange={(s) => set('channel', s)}
        options={options(refList(ref, 'intake_channel'))}
        placeholder="How did it arrive?"
        required
      />
      <SelectField
        label="Island of origin"
        value={v.origin_island}
        onChange={(s) => set('origin_island', s)}
        options={options(refList(ref, 'origin_island'))}
        placeholder="Not recorded"
        help="The island of origin does not decide jurisdiction."
      />
      <DateField
        label="Document date"
        value={v.document_date}
        onChange={(s) => set('document_date', s)}
        help="The date on the document itself — may differ from when it arrived."
      />
      <DateField
        label="Received date"
        value={v.received_date}
        onChange={(s) => set('received_date', s)}
        required
        max={courtToday()}
        help="When the registry actually received it (court date)."
      />
      <TextArea
        label="Description"
        value={v.description}
        onChange={(s) => set('description', s)}
        required
        rows={4}
        help="What was filed, in plain words (e.g. claim about an unfulfilled agreement)."
      />
      <CheckboxField
        label="Paper original received"
        checked={v.is_paper_original}
        onChange={(b) => set('is_paper_original', b)}
      />
      {v.is_paper_original && (
        <TextField
          label="Where the paper original is kept"
          value={v.paper_location}
          onChange={(s) => set('paper_location', s)}
          required
          error={v.is_paper_original && localError && !v.paper_location.trim() ? 'Required when a paper original was received.' : undefined}
          help="A scan is not the paper original — record the shelf, folder or office."
        />
      )}
      <div className="actions">
        <Button type="submit" busy={busy}>
          {submitLabel}
        </Button>
        {onCancel && (
          <Button type="button" variant="secondary" onClick={onCancel}>
            Cancel
          </Button>
        )}
      </div>
      </fieldset>
    </form>
    </>
  );
}
