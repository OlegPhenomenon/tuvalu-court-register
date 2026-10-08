/**
 * DispatchForm — prepare a notice or a copy package for ONE case participant
 * (create), or edit a draft dispatch (PATCH with optimistic locking).
 *
 * Create: POST /api/cases/{id}/dispatches — the server renders subject/body
 * from the chosen template (+ hearing variables) or the copy_dispatch
 * template with the exact item inventory.
 * Edit:   PATCH /api/dispatches/{id} — editable fields only; a draft edit
 * clears the recorded review server-side.
 *
 * Copy packages list every clean, visible document version. Judicial notes
 * are never offered; restricted versions appear only behind an explicit
 * "Include restricted document" checkbox with a warning (spec §3 step 9, C12).
 */

import { useEffect, useMemo, useRef, useState } from 'react';
import type { FormEvent } from 'react';
import { api } from '../api';
import { fmtCourtLocal, fmtLocal } from '../time';
import { Button } from './Button';
import { ErrorBanner } from './ErrorBanner';
import { CheckboxField, SelectField, TextArea, TextField } from './fields';
import { Modal } from './Modal';
import { label, options, refList, useRef as useRefData } from './refdata';
import { useApi } from './useApi';
import { FormErrors } from '../pages/intake/FormErrors';
import type { DispatchRecord } from './DispatchCard';

/** Minimal participant shape needed here — compatible with case/types Participant. */
export interface FormParticipant {
  id: number;
  party_id: number;
  name: string;
  role: string;
  active: number;
  service_contact: string | null;
  contact_email: string | null;
  address: string | null;
}

/** Default dispatch address of a participant and where it came from (same order as the server). */
function addressSource(p: FormParticipant): { value: string; label: string } | null {
  const first = (v: string | null) => (v && v.trim() ? v : null);
  const service = first(p.service_contact);
  if (service) return { value: service, label: 'Service contact' };
  const email = first(p.contact_email);
  if (email) return { value: email, label: 'E-mail on the party record' };
  const postal = first(p.address);
  if (postal) return { value: postal, label: 'Address on the party record' };
  return null;
}

interface DocListItem {
  id: number;
  title: string;
  doc_type: string;
  visibility: string;
}

interface DocVersion {
  id: number;
  version_no: number;
  filename: string;
  scan_status: string;
}

interface DocDetail extends DocListItem {
  versions: DocVersion[];
}

interface VersionOption {
  version_id: number;
  label: string;
  restricted: boolean;
}

interface HearingOption {
  id: number;
  hearing_type_label?: string;
  hearing_type: string;
  starts_local?: string;
  starts_at: string;
  room_name: string | null;
  status: string;
}

const STATUS_WORD: Record<string, string> = {
  draft: 'draft', scheduled: 'scheduled', held: 'held', adjourned: 'adjourned', cancelled: 'cancelled',
};

/** Load the case's visible document versions (clean scans only) for the copies checklist. */
function useCaseVersions(caseId: number | null, needed: boolean) {
  const [versions, setVersions] = useState<VersionOption[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    if (!needed || caseId === null) return;
    let alive = true;
    setError(null);
    void (async () => {
      try {
        const list = await api<{ items: DocListItem[] }>('GET', `/cases/${caseId}/documents`);
        // allSettled: a document that vanished between list and detail must not
        // break the whole checklist.
        const details = await Promise.allSettled(
          list.items.map((d) => api<DocDetail>('GET', `/documents/${d.id}`)),
        );
        if (list.items.length > 0 && details.every((r) => r.status === 'rejected')) {
          throw (details[0] as PromiseRejectedResult).reason;
        }
        const opts: VersionOption[] = [];
        for (const r of details) {
          if (r.status !== 'fulfilled') continue;
          const doc = r.value;
          // Judicial working notes are never sendable — not offered at all.
          if (doc.visibility === 'judicial_note' || doc.doc_type === 'judicial_note') continue;
          for (const v of doc.versions ?? []) {
            if (v.scan_status !== 'clean') continue;
            opts.push({
              version_id: v.id,
              label: `${doc.title} — version ${v.version_no} (${v.filename})`,
              restricted: doc.visibility === 'restricted',
            });
          }
        }
        if (alive) setVersions(opts);
      } catch (e) {
        if (alive) setError(e);
      }
    })();
    return () => {
      alive = false;
    };
  }, [caseId, needed, tick]);

  return { versions, error, retry: () => setTick((t) => t + 1) };
}

export function DispatchForm({ caseId, participants, kind, dispatch, preselectDecisionId, preselectPartyId, onClose, onSaved }: {
  /** Create mode: the owning case. */
  caseId?: number;
  /** Create mode: active case participants to pick the recipient from. */
  participants?: FormParticipant[];
  /** Create mode: what to prepare. */
  kind?: 'notice' | 'copies';
  /** Edit mode: the draft dispatch being edited. */
  dispatch?: DispatchRecord;
  /** Create copies mode: pre-check the version bound to this decision (?decision=). */
  preselectDecisionId?: number;
  /** Create mode: pre-choose the recipient participant by party id (?party=). */
  preselectPartyId?: number;
  onClose: () => void;
  /** `fresh` = updated record returned by the server. */
  onSaved: (fresh?: DispatchRecord) => void;
}) {
  const form = useRef<HTMLFormElement>(null);
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const editing = dispatch !== undefined;
  const effectiveKind: 'notice' | 'copies' | string = editing ? dispatch.kind : (kind ?? 'notice');
  const isCopies = effectiveKind === 'copies';
  const isNotice = !editing && effectiveKind === 'notice';
  const editCaseId = editing ? dispatch.case_id : (caseId ?? null);

  // ------------------------------ create state ------------------------------
  const [partyId, setPartyId] = useState('');
  const [template, setTemplate] = useState('');
  const [hearingId, setHearingId] = useState('');
  const [purpose, setPurpose] = useState('');

  // ------------------------------ shared state ------------------------------
  const [recipientName, setRecipientName] = useState(dispatch?.recipient_name ?? '');
  const [method, setMethod] = useState(dispatch?.method ?? 'email');
  const [address, setAddress] = useState(dispatch?.address ?? '');
  const [subject, setSubject] = useState(dispatch?.subject ?? '');
  const [body, setBody] = useState(dispatch?.body ?? '');
  const [selected, setSelected] = useState<number[]>(
    () => dispatch?.items.map((i) => i.document_version_id) ?? [],
  );
  const [includeRestricted, setIncludeRestricted] = useState(
    () => dispatch?.items.some((i) => i.visibility === 'restricted') ?? false,
  );
  const [version, setVersion] = useState(dispatch?.version ?? 0);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [attempted, setAttempted] = useState<Record<string, unknown> | undefined>(undefined);
  const [localError, setLocalError] = useState('');

  const hearings = useApi<{ items: HearingOption[] }>(
    isNotice && caseId !== undefined ? `/cases/${caseId}/hearings` : null,
  );
  const { versions, error: versionsError, retry: retryVersions } = useCaseVersions(editCaseId, isCopies);

  // ?party={party_id} — pre-choose the recipient once, without locking the field.
  const preselectedParty = useRef(false);
  useEffect(() => {
    if (editing || preselectedParty.current || preselectPartyId == null) return;
    preselectedParty.current = true;
    const p = (participants ?? []).find((x) => x.party_id === preselectPartyId && x.active);
    if (p) {
      setPartyId(String(p.id));
      setAddress(addressSource(p)?.value ?? '');
    }
  }, [editing, participants, preselectPartyId]);

  // ?decision={id} — resolve the decision to its bound document version and
  // pre-check it once the versions list arrives (only if it is sendable here).
  const decisions = useApi<{ items: { id: number; document_version_id: number }[] }>(
    !editing && isCopies && preselectDecisionId != null && caseId !== undefined
      ? `/cases/${caseId}/decisions`
      : null,
  );
  useEffect(() => {
    if (preselectDecisionId == null || !versions) return;
    const vid = decisions.data?.items.find((d) => d.id === preselectDecisionId)?.document_version_id;
    if (vid != null && versions.some((v) => v.version_id === vid)) {
      setSelected((s) => (s.includes(vid) ? s : [vid, ...s]));
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [versions, decisions.data]);

  const chosen = (participants ?? []).find((p) => String(p.id) === partyId);
  const chosenAddress = chosen ? addressSource(chosen) : null;
  const pickRecipient = (value: string) => {
    setPartyId(value);
    const p = (participants ?? []).find((x) => String(x.id) === value);
    setAddress(p ? (addressSource(p)?.value ?? '') : '');
  };

  const toggleVersion = (id: number, checked: boolean) =>
    setSelected((s) => (checked ? [...s, id] : s.filter((x) => x !== id)));

  const { restrictedOptions, openVersions } = useMemo(() => {
    const all = versions ?? [];
    return {
      restrictedOptions: all.filter((v) => v.restricted),
      openVersions: all.filter((v) => !v.restricted),
    };
  }, [versions]);

  // Unticking the restricted gate drops any restricted versions from the selection.
  useEffect(() => {
    if (includeRestricted) return;
    setSelected((s) => {
      const next = s.filter((id) => !restrictedOptions.some((r) => r.version_id === id));
      return next.length === s.length ? s : next;
    });
  }, [includeRestricted, restrictedOptions]);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setLocalError('');
    setError(null);
    if (editing) {
      const payload: Record<string, unknown> = {
        version,
        recipient_name: recipientName,
        method,
        address: address.trim() || null,
        subject,
        body,
      };
      if (isCopies) {
        if (selected.length === 0) {
          setLocalError('Choose at least one document version to send.');
          return;
        }
        payload.version_ids = selected;
        payload.include_restricted = includeRestricted;
      }
      setAttempted(payload);
      setBusy(true);
      try {
        const fresh = await api<DispatchRecord>('PATCH', `/dispatches/${dispatch.id}`, payload);
        onSaved(fresh);
      } catch (err) {
        setError(err);
      } finally {
        setBusy(false);
      }
      return;
    }

    // ------------------------------ create ------------------------------
    if (!partyId || !chosen) {
      setLocalError('Choose the recipient — one package per participant.');
      return;
    }
    if (isCopies && selected.length === 0) {
      setLocalError('Choose at least one document version to send.');
      return;
    }
    const payload: Record<string, unknown> = {
      kind: effectiveKind,
      recipient_party_id: chosen.party_id,
      method,
      address: address.trim() || null,
      purpose: purpose.trim() || null,
    };
    if (effectiveKind === 'notice') {
      payload.template_code = template || null;
      payload.hearing_id = hearingId ? Number(hearingId) : null;
    }
    if (isCopies) {
      payload.version_ids = selected;
      payload.include_restricted = includeRestricted;
    }
    setAttempted(payload);
    setBusy(true);
    try {
      const fresh = await api<DispatchRecord>('POST', `/cases/${caseId}/dispatches`, payload);
      onSaved(fresh);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  const title = editing
    ? `Edit dispatch to ${dispatch.recipient_name}`
    : effectiveKind === 'copies'
      ? 'Prepare a copy package'
      : 'Prepare a notice';

  return (
    <Modal title={title} open onClose={busy ? () => {} : onClose}>
      <FormErrors
        error={error}
        form={form}
        attempted={attempted}
        onReviewVersion={(v) => {
          setVersion(v);
          setError(null);
        }}
      />
      <ErrorBanner error={refError} onRetry={reloadRef} />
      {localError && (
        <div className="banner banner--error" role="alert">
          <p>{localError}</p>
        </div>
      )}
      <form ref={form} onSubmit={submit}>
        <fieldset disabled={busy} style={{ border: 0, margin: 0, padding: 0, minWidth: 0 }}>
          {editing ? (
            <TextField
              label="Recipient name"
              value={recipientName}
              onChange={setRecipientName}
              required
            />
          ) : (
            <SelectField
              label="Recipient"
              value={partyId}
              onChange={pickRecipient}
              options={(participants ?? [])
                .filter((p) => p.active)
                .map((p) => ({
                  value: String(p.id),
                  label: `${p.name} — ${label(refList(ref, 'participant_role'), p.role)}`,
                }))}
              placeholder="Choose a participant"
              required
              help={
                chosen
                  ? chosenAddress
                    ? `Default address from: ${chosenAddress.label}`
                    : 'No address on record — enter the address by hand.'
                  : 'Make a separate package for each participant.'
              }
            />
          )}

          <SelectField
            label="Delivery method"
            value={method}
            onChange={setMethod}
            options={options(refList(ref, 'dispatch_method'))}
            required
          />
          <TextField
            label="Address"
            value={address}
            onChange={setAddress}
            required={method === 'email'}
            help={
              method === 'email'
                ? 'Required for e-mail — delivered to the local mailbox in this installation.'
                : 'Postal address, registry desk or officer routing for this message.'
            }
          />

          {isNotice && (
            <>
              <SelectField
                label="Template"
                value={template}
                onChange={setTemplate}
                options={(ref?.templates ?? []).map((t) => ({ value: t.code, label: t.name }))}
                placeholder="Choose a template"
                required
                help="The server fills in the subject and text — a human reviews the result before sending."
              />
              <SelectField
                label="Hearing (optional)"
                value={hearingId}
                onChange={setHearingId}
                options={(hearings.data?.items ?? []).map((h) => ({
                  value: String(h.id),
                  label: `${h.hearing_type_label ?? h.hearing_type} — ${
                    h.starts_local ? fmtCourtLocal(h.starts_local) : fmtLocal(h.starts_at)
                  }${h.room_name ? `, ${h.room_name}` : ''} (${STATUS_WORD[h.status] ?? h.status})`,
                }))}
                placeholder="No hearing linked"
              />
            </>
          )}

          {!editing && (
            <TextField
              label="Purpose (optional)"
              value={purpose}
              onChange={setPurpose}
              help="Recorded with the package, e.g. service of process, party copy."
            />
          )}

          {editing && (
            <>
              <TextField label="Subject" value={subject} onChange={setSubject} required />
              <TextArea label="Message text" value={body} onChange={setBody} required rows={8} />
            </>
          )}

          {isCopies && (
            <div className="field">
              <span className="field-label">Document versions to include</span>
              {Boolean(versionsError) && <ErrorBanner error={versionsError} onRetry={retryVersions} />}
              {!versions && !versionsError && <p className="muted">Loading documents…</p>}
              {versions && versions.length === 0 && (
                <p className="muted">No clean, visible document versions on this case.</p>
              )}
              {openVersions.length > 0 && (
                <ul className="doc-checklist">
                  {openVersions.map((v) => (
                    <li key={v.version_id}>
                      <label>
                        <input
                          type="checkbox"
                          checked={selected.includes(v.version_id)}
                          onChange={(e) => toggleVersion(v.version_id, e.target.checked)}
                        />{' '}
                        {v.label}
                      </label>
                    </li>
                  ))}
                </ul>
              )}
              {restrictedOptions.length > 0 && (
                <>
                  <CheckboxField
                    label="Include restricted documents"
                    checked={includeRestricted}
                    onChange={setIncludeRestricted}
                    help="Restricted material leaves the registry only by explicit choice — check who may receive it."
                  />
                  {includeRestricted && (
                    <div className="banner doc-warning" role="alert">
                      <p>
                        <strong>Restricted documents selected for dispatch.</strong> Sending them is
                        a deliberate disclosure — the choice is recorded with the package.
                      </p>
                      <ul className="doc-checklist">
                        {restrictedOptions.map((v) => (
                          <li key={v.version_id}>
                            <label>
                              <input
                                type="checkbox"
                                checked={selected.includes(v.version_id)}
                                onChange={(e) => toggleVersion(v.version_id, e.target.checked)}
                              />{' '}
                              {v.label} <span className="badge badge--danger">Restricted</span>
                            </label>
                          </li>
                        ))}
                      </ul>
                    </div>
                  )}
                </>
              )}
              <p className="field-help">
                Judicial working notes are never offered here. Quarantined files cannot be sent.
              </p>
            </div>
          )}

          <div className="actions">
            <Button type="submit" busy={busy}>
              {editing ? 'Save changes' : effectiveKind === 'copies' ? 'Prepare the package' : 'Prepare the notice'}
            </Button>
            <Button type="button" variant="secondary" onClick={onClose}>
              Cancel
            </Button>
          </div>
        </fieldset>
      </form>
    </Modal>
  );
}
