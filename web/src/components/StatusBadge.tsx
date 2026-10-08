type Tone = 'neutral' | 'info' | 'success' | 'warn' | 'danger';

// Every status from ARCHITECTURE.md §5 (intake / case / hearing / decision /
// dispatch / task) → human label + colour tone.
const MAP: Record<string, { label: string; tone: Tone }> = {
  received: { label: 'Received', tone: 'info' },
  needs_information: { label: 'Waiting for information', tone: 'warn' },
  ready_for_registration: { label: 'Ready for registration', tone: 'success' },
  linked_to_case: { label: 'Linked to case', tone: 'success' },
  returned_or_redirected: { label: 'Returned or redirected', tone: 'neutral' },
  duplicate: { label: 'Duplicate', tone: 'neutral' },
  registered: { label: 'Registered', tone: 'info' },
  active: { label: 'Active', tone: 'success' },
  on_hold: { label: 'On hold', tone: 'warn' },
  closed: { label: 'Closed', tone: 'neutral' },
  reopened: { label: 'Reopened', tone: 'info' },
  draft: { label: 'Draft', tone: 'neutral' },
  scheduled: { label: 'Scheduled', tone: 'info' },
  held: { label: 'Held', tone: 'success' },
  adjourned: { label: 'Adjourned', tone: 'warn' },
  cancelled: { label: 'Cancelled', tone: 'neutral' },
  finalised: { label: 'Finalised', tone: 'success' },
  superseded: { label: 'Superseded', tone: 'neutral' },
  queued: { label: 'Queued', tone: 'info' },
  sent: { label: 'Sent', tone: 'success' },
  failed: { label: 'Failed', tone: 'danger' },
  open: { label: 'Open', tone: 'info' },
  done: { label: 'Done', tone: 'success' },
  carried_forward: { label: 'Carried forward', tone: 'warn' },
};

/** Status pill for any state-machine status in ARCHITECTURE.md §5: `<StatusBadge status={row.status} />`. */
export function StatusBadge({ status }: { status: string }) {
  const m = MAP[status] ?? { label: status.replace(/_/g, ' '), tone: 'neutral' as Tone };
  return <span className={`badge badge--${m.tone}`}>{m.label}</span>;
}
