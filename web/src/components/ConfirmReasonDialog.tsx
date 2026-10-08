import { useEffect, useState } from 'react';
import { Button } from './Button';
import { TextArea } from './fields';
import { Modal } from './Modal';

/**
 * Modal that demands a non-empty reason before confirming (adjourn, cancel,
 * carry forward, reopen, administrative corrections…):
 * `<ConfirmReasonDialog open title="Cancel hearing" label="Reason" onConfirm={r=>…} onClose={…} />`.
 */
export function ConfirmReasonDialog({ open, title, label, confirmLabel = 'Confirm', danger, busy, onConfirm, onClose }: {
  open: boolean;
  title: string;
  label: string;
  confirmLabel?: string;
  /** Render the confirm button in danger styling. */
  danger?: boolean;
  busy?: boolean;
  onConfirm: (reason: string) => void;
  onClose: () => void;
}) {
  const [reason, setReason] = useState('');
  useEffect(() => {
    if (open) setReason('');
  }, [open]);

  const trimmed = reason.trim();
  return (
    <Modal title={title} open={open} onClose={onClose}>
      <TextArea label={label} value={reason} onChange={setReason} required rows={3} autoFocus />
      <div className="actions">
        <Button
          variant={danger ? 'danger' : 'primary'}
          busy={busy}
          disabled={!trimmed}
          onClick={() => onConfirm(trimmed)}
        >
          {confirmLabel}
        </Button>
        <Button variant="secondary" onClick={onClose}>Cancel</Button>
      </div>
    </Modal>
  );
}
