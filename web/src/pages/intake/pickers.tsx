/**
 * Debounced search pickers shared by the intake and case screens:
 * pick an existing party, case or filing instead of retyping it.
 * Picking a record is always explicit — the register never merges by name.
 */

import { useCallback, useEffect, useId, useState } from 'react';
import type { ReactNode } from 'react';
import { ErrorBanner } from '../../components/ErrorBanner';
import { api } from '../../api';
import { StatusBadge } from '../../components/StatusBadge';
import { fmtDate } from '../../time';

function useDebounced<T>(value: T, ms = 300): T {
  const [v, setV] = useState(value);
  useEffect(() => {
    const t = setTimeout(() => setV(value), ms);
    return () => clearTimeout(t);
  }, [value, ms]);
  return v;
}

export interface Party {
  id: number;
  kind: string;
  name: string;
  contact_email?: string | null;
  contact_phone?: string | null;
  address?: string | null;
  island?: string | null;
}

export interface CaseHit {
  id: number;
  number: string;
  title: string;
  status: string;
}

export interface IntakeHit {
  id: number;
  reference: string;
  sender_name: string;
  status: string;
  received_date: string;
}

interface SearchSelectProps<T> {
  label: string;
  selected: T | null;
  onSelect: (item: T | null) => void;
  search: (q: string) => Promise<T[]>;
  itemKey: (item: T) => string | number;
  itemLabel: (item: T) => ReactNode;
  selectedLabel?: (item: T) => ReactNode;
  placeholder?: string;
  help?: string;
  required?: boolean;
}

function SearchSelect<T>({
  label: fieldLabel,
  selected,
  onSelect,
  search,
  itemKey,
  itemLabel,
  selectedLabel,
  placeholder,
  help,
  required,
}: SearchSelectProps<T>) {
  const id = useId();
  const [query, setQuery] = useState('');
  const debounced = useDebounced(query);
  const [results, setResults] = useState<T[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [tick, setTick] = useState(0);
  const [searching, setSearching] = useState(false);

  useEffect(() => {
    const q = debounced.trim();
    setError(null);
    setResults(null);
    if (q.length < 2) {
      setResults(null);
      setSearching(false);
      return;
    }
    let alive = true;
    setSearching(true);
    search(q)
      .then((r) => {
        if (alive) setResults(r);
      })
      .catch((error) => {
        if (alive) setError(error);
      })
      .finally(() => {
        if (alive) setSearching(false);
      });
    return () => {
      alive = false;
    };
  }, [debounced, search, tick]);

  if (selected) {
    return (
      <div className="field">
        <span className="field-label">
          {fieldLabel}
          {required && (
            <span className="req" aria-hidden="true">
              {' '}
              *
            </span>
          )}
        </span>
        <p style={{ display: 'flex', alignItems: 'center', gap: '0.5rem', flexWrap: 'wrap' }}>
          <strong>{selectedLabel ? selectedLabel(selected) : itemLabel(selected)}</strong>{' '}
          <button type="button" className="btn btn--secondary" onClick={() => onSelect(null)}>
            Change
          </button>
        </p>
        {help && <p className="field-help" id={`${id}-help`}>{help}</p>}
      </div>
    );
  }

  return (
    <div className="field">
      <label className="field-label" htmlFor={id}>
        {fieldLabel}
        {required && (
          <span className="req" aria-hidden="true">
            {' '}
            *
          </span>
        )}
      </label>
      <input
        className="input"
        id={id}
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        placeholder={placeholder ?? 'Type at least 2 characters to search'}
        autoComplete="off"
        required={required}
        aria-describedby={help ? `${id}-help` : undefined}
      />
      {help && <p className="field-help" id={`${id}-help`}>{help}</p>}
      <div onClick={(e) => e.preventDefault()}><ErrorBanner error={error} onRetry={() => setTick((t) => t + 1)} /></div>
      {searching && <p className="field-help">Searching…</p>}
      {!searching && results !== null && results.length === 0 && <p className="field-help">No matches.</p>}
      {results && results.length > 0 && (
        <ul
          id={`${id}-results`}
          aria-label={`${fieldLabel} search results`}
          style={{
            listStyle: 'none',
            margin: '0.25rem 0 0',
            padding: '0.25rem',
            border: '1px solid var(--line)',
            borderRadius: '6px',
            background: 'var(--surface)',
          }}
        >
          {results.map((item) => (
            <li key={itemKey(item)}>
              <button
                type="button"
                onClick={() => onSelect(item)}
                style={{
                  font: 'inherit',
                  display: 'block',
                  width: '100%',
                  textAlign: 'left',
                  background: 'none',
                  border: 'none',
                  padding: '0.4rem 0.5rem',
                  borderRadius: '4px',
                  cursor: 'pointer',
                }}
              >
                {itemLabel(item)}
              </button>
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

const searchParties = async (q: string) => {
  const res = await api<{ items: Party[] }>('GET', `/parties?q=${encodeURIComponent(q)}`);
  return res.items;
};

const searchCases = async (q: string) => {
  const res = await api<{ items: CaseHit[] }>('GET', `/cases?q=${encodeURIComponent(q)}`);
  return res.items;
};

/** Pick an existing party record (person or organisation). */
export function PartyPicker({
  value,
  onChange,
  label = 'Party',
  required,
  help,
}: {
  value: Party | null;
  onChange: (p: Party | null) => void;
  label?: string;
  required?: boolean;
  help?: string;
}) {
  return (
    <SearchSelect<Party>
      label={label}
      required={required}
      help={help}
      selected={value}
      onSelect={onChange}
      search={searchParties}
      itemKey={(p) => p.id}
      itemLabel={(p) => (
        <>
          {p.name} <span className="muted">({p.kind}{p.island ? `, ${p.island}` : ''}{p.contact_email ? `, ${p.contact_email}` : ''})</span>
        </>
      )}
      selectedLabel={(p) => `${p.name} (${p.kind})`}
    />
  );
}

/** Pick an existing case (search by number, title or party). */
export function CasePicker({
  value,
  onChange,
  label = 'Case',
  required,
  help,
}: {
  value: CaseHit | null;
  onChange: (c: CaseHit | null) => void;
  label?: string;
  required?: boolean;
  help?: string;
}) {
  return (
    <SearchSelect<CaseHit>
      label={label}
      required={required}
      help={help}
      selected={value}
      onSelect={onChange}
      search={searchCases}
      itemKey={(c) => c.id}
      itemLabel={(c) => (
        <>
          <strong>{c.number}</strong> — {c.title} <StatusBadge status={c.status} />
        </>
      )}
      selectedLabel={(c) => `${c.number} — ${c.title}`}
    />
  );
}

/** Pick another filing (for "mark as duplicate of …"); `excludeId` hides the current one. */
export function IntakePicker({
  value,
  onChange,
  excludeId,
  label = 'Filing',
  required,
}: {
  value: IntakeHit | null;
  onChange: (i: IntakeHit | null) => void;
  excludeId?: number;
  label?: string;
  required?: boolean;
}) {
  // Stable identity: SearchSelect re-fetches whenever `search` changes.
  const search = useCallback(
    async (q: string) => {
      const res = await api<{ items: IntakeHit[] }>('GET', `/intakes?q=${encodeURIComponent(q)}`);
      return res.items.filter((i) => i.id !== excludeId);
    },
    [excludeId],
  );
  return (
    <SearchSelect<IntakeHit>
      label={label}
      required={required}
      selected={value}
      onSelect={onChange}
      search={search}
      itemKey={(i) => i.id}
      itemLabel={(i) => (
        <>
          <strong>{i.reference}</strong> — {i.sender_name} <StatusBadge status={i.status} />{' '}
          <span className="muted">received {fmtDate(i.received_date)}</span>
        </>
      )}
      selectedLabel={(i) => `${i.reference} — ${i.sender_name}`}
    />
  );
}
