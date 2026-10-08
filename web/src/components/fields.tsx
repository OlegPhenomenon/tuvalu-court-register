/**
 * Form fields with a visible <label>, `required` marker, help text and
 * `role="alert"` error text. `onChange` delivers the plain value (string /
 * boolean), not the DOM event. Any extra input attributes (autoComplete,
 * inputMode, min, max, placeholder, disabled, …) pass through to the control.
 *
 * DateTimeField values are court-local "YYYY-MM-DDTHH:MM" strings, passed
 * to the server as-is — never convert with the browser timezone.
 */

import { useId } from 'react';
import type { InputHTMLAttributes, ReactNode, SelectHTMLAttributes, TextareaHTMLAttributes } from 'react';

interface CommonProps {
  label: string;
  error?: string;
  help?: string;
  required?: boolean;
}

function useMeta(error?: string, help?: string) {
  const id = useId();
  const errorId = `${id}-error`;
  const helpId = `${id}-help`;
  return {
    id,
    errorId,
    helpId,
    describedBy: error ? errorId : help ? helpId : undefined,
  };
}

function Field({ id, label, required, error, help, errorId, helpId, children }: CommonProps & {
  id: string;
  errorId: string;
  helpId: string;
  children: ReactNode;
}) {
  return (
    <div className={error ? 'field field--error' : 'field'}>
      <label className="field-label" htmlFor={id}>
        {label}
        {required && <span className="req" aria-hidden="true"> *</span>}
      </label>
      {children}
      {help && <p className="field-help" id={helpId}>{help}</p>}
      {error && <p className="field-error" id={errorId} role="alert">{error}</p>}
    </div>
  );
}

/** `<TextField label="Username" value onChange={setV} />` — pass `type="password"` etc. via props. */
export function TextField({ label, error, help, required, onChange, ...input }: CommonProps &
  Omit<InputHTMLAttributes<HTMLInputElement>, 'onChange' | 'id'> & { onChange?: (value: string) => void }) {
  const m = useMeta(error, help);
  return (
    <Field id={m.id} label={label} required={required} error={error} help={help} errorId={m.errorId} helpId={m.helpId}>
      <input
        className="input"
        id={m.id}
        required={required}
        aria-invalid={error ? true : undefined}
        aria-describedby={m.describedBy}
        onChange={(e) => onChange?.(e.target.value)}
        {...input}
      />
    </Field>
  );
}

/** Multi-line text: `<TextArea label="Reason" rows={4} value onChange={setV} />`. */
export function TextArea({ label, error, help, required, onChange, ...input }: CommonProps &
  Omit<TextareaHTMLAttributes<HTMLTextAreaElement>, 'onChange' | 'id'> & { onChange?: (value: string) => void }) {
  const m = useMeta(error, help);
  return (
    <Field id={m.id} label={label} required={required} error={error} help={help} errorId={m.errorId} helpId={m.helpId}>
      <textarea
        className="input"
        id={m.id}
        required={required}
        aria-invalid={error ? true : undefined}
        aria-describedby={m.describedBy}
        onChange={(e) => onChange?.(e.target.value)}
        {...input}
      />
    </Field>
  );
}

/** `<SelectField label="Status" options={[{value,label}]} value onChange={setV} />`. */
export function SelectField({ label, error, help, required, onChange, options, placeholder, ...input }: CommonProps &
  Omit<SelectHTMLAttributes<HTMLSelectElement>, 'onChange' | 'id'> & {
    onChange?: (value: string) => void;
    options: { value: string; label: string }[];
    /** Shown as a disabled empty option; also lets an empty `value` render. */
    placeholder?: string;
  }) {
  const m = useMeta(error, help);
  return (
    <Field id={m.id} label={label} required={required} error={error} help={help} errorId={m.errorId} helpId={m.helpId}>
      <select
        className="input"
        id={m.id}
        required={required}
        aria-invalid={error ? true : undefined}
        aria-describedby={m.describedBy}
        onChange={(e) => onChange?.(e.target.value)}
        {...input}
      >
        {placeholder !== undefined && <option value="" disabled={required}>{placeholder}</option>}
        {options.map((o) => <option key={o.value} value={o.value}>{o.label}</option>)}
      </select>
    </Field>
  );
}

/** Court-local calendar date, "YYYY-MM-DD". */
export function DateField({ label, error, help, required, onChange, ...input }: CommonProps &
  Omit<InputHTMLAttributes<HTMLInputElement>, 'onChange' | 'id' | 'type'> & { onChange?: (value: string) => void }) {
  const m = useMeta(error, help);
  return (
    <Field id={m.id} label={label} required={required} error={error} help={help} errorId={m.errorId} helpId={m.helpId}>
      <input
        type="date"
        className="input"
        id={m.id}
        required={required}
        aria-invalid={error ? true : undefined}
        aria-describedby={m.describedBy}
        onChange={(e) => onChange?.(e.target.value)}
        {...input}
      />
    </Field>
  );
}

/** Court-local date+time, "YYYY-MM-DDTHH:MM" — sent to the server verbatim (no TZ conversion). */
export function DateTimeField({ label, error, help, required, onChange, ...input }: CommonProps &
  Omit<InputHTMLAttributes<HTMLInputElement>, 'onChange' | 'id' | 'type'> & { onChange?: (value: string) => void }) {
  const m = useMeta(error, help);
  return (
    <Field id={m.id} label={label} required={required} error={error} help={help} errorId={m.errorId} helpId={m.helpId}>
      <input
        type="datetime-local"
        className="input"
        id={m.id}
        required={required}
        aria-invalid={error ? true : undefined}
        aria-describedby={m.describedBy}
        onChange={(e) => onChange?.(e.target.value)}
        {...input}
      />
    </Field>
  );
}

/** `<CheckboxField label="Restricted" checked={v} onChange={setV} />` — label sits beside the box. */
export function CheckboxField({ label, error, help, required, onChange, ...input }: CommonProps &
  Omit<InputHTMLAttributes<HTMLInputElement>, 'onChange' | 'id' | 'type'> & { onChange?: (checked: boolean) => void }) {
  const m = useMeta(error, help);
  return (
    <div className={error ? 'field field--check field--error' : 'field field--check'}>
      <div className="check-row">
        <input
          type="checkbox"
          id={m.id}
          required={required}
          aria-invalid={error ? true : undefined}
          aria-describedby={m.describedBy}
          onChange={(e) => onChange?.(e.target.checked)}
          {...input}
        />
        <label htmlFor={m.id}>{label}</label>
      </div>
      {help && <p className="field-help" id={m.helpId}>{help}</p>}
      {error && <p className="field-error" id={m.errorId} role="alert">{error}</p>}
    </div>
  );
}
