import { useEffect, useId, useRef, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { api } from '../api';
import { useSession } from '../session';
import { ErrorBanner } from './ErrorBanner';
import { Icon } from './icons';
import '../pages/admin.css';

type Result = { id: number; link: string; number?: string; title?: string; case_number?: string | null; reference?: string; sender_name?: string };
type Results = { cases: Result[]; documents: Result[]; intakes: Result[] };
const groups = [['cases', 'Cases'], ['documents', 'Documents'], ['intakes', 'Filings']] as const;
const text = (r: Result) => [r.number || r.reference || r.case_number, r.title || r.sender_name].filter(Boolean).join(' — ');
export default function GlobalSearch() {
  const { session } = useSession();
  const navigate = useNavigate();
  const id = useId();
  const container = useRef<HTMLDivElement>(null);
  const [q, setQ] = useState('');
  const [results, setResults] = useState<Results | null>(null);
  const [open, setOpen] = useState(false);
  const [active, setActive] = useState(-1);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(false);
  const [retry, setRetry] = useState(0);
  useEffect(() => { setQ(''); setOpen(false); setResults(null); }, [session]);
  useEffect(() => {
    const ac = new AbortController();
    setResults(null); setError(null); setActive(-1);
    if (q.trim().length < 2) { setLoading(false); return; }
    setLoading(true);
    const timer = setTimeout(() => {
      api<Results>('GET', `/search?q=${encodeURIComponent(q.trim())}`, undefined, { signal: ac.signal }).then(r => { if (!ac.signal.aborted) setResults(r); }).catch(e => { if (!ac.signal.aborted) setError(e); }).finally(() => { if (!ac.signal.aborted) setLoading(false); });
    }, 300);
    return () => { clearTimeout(timer); ac.abort(); };
  }, [q, retry, session]);
  useEffect(() => { const close = (e: PointerEvent) => { if (!container.current?.contains(e.target as Node)) setOpen(false); }; document.addEventListener('pointerdown', close); return () => document.removeEventListener('pointerdown', close); }, []);
  useEffect(() => { if (active >= 0) document.getElementById(`${id}-${active}`)?.scrollIntoView({ block: 'nearest' }); }, [active, id]);
  const rows = results ? groups.flatMap(([key]) => results[key]) : [];
  const choose = (r: Result) => { setOpen(false); setQ(''); navigate(r.link); };
  let index = 0;
  return <div className="global-search" ref={container} onBlur={e => { if (!e.currentTarget.contains(e.relatedTarget as Node)) setOpen(false); }}>
    <label className="sr-only" htmlFor={id}>Search cases, documents and filings</label>
    <Icon name="search" size={17} className="global-search-icon" />
    <input className="input" id={id} value={q} autoComplete="off" role="combobox" aria-autocomplete="list" aria-expanded={open && q.trim().length >= 2} aria-controls={`${id}-list`} aria-activedescendant={open && active >= 0 ? `${id}-${active}` : undefined} placeholder="Number, name or title…" onFocus={() => setOpen(true)} onChange={e => { setQ(e.target.value); setOpen(true); }} onKeyDown={e => {
      if (e.key === 'Escape') { e.preventDefault(); setOpen(false); setActive(-1); }
      if (e.key === 'ArrowDown' || e.key === 'ArrowUp') { e.preventDefault(); setOpen(true); if (rows.length) setActive(a => e.key === 'ArrowDown' ? (a + 1) % rows.length : a <= 0 ? rows.length - 1 : a - 1); }
      if (e.key === 'Enter' && open && rows[active]) { e.preventDefault(); choose(rows[active]); }
    }} />
    {open && q.trim().length >= 2 && <div className="search-dropdown">
      <ErrorBanner error={error} onRetry={() => setRetry(t => t + 1)} />
      {loading && <p role="status">Searching…</p>}
      {!loading && !error && results && rows.length === 0 && <p role="status">No permitted matches.</p>}
      <div id={`${id}-list`} role="listbox" aria-label="Search results">{results && groups.map(([key, title]) => results[key].length > 0 && <div key={key} role="group" aria-label={title}><h3>{title}</h3>{results[key].map(r => { const n = index++; return <div key={r.id} id={`${id}-${n}`} role="option" aria-selected={active === n} className={`search-result${active === n ? ' is-active' : ''}`} onMouseDown={e => e.preventDefault()} onMouseMove={() => setActive(n)} onClick={() => choose(r)}>{text(r)}</div>; })}</div>)}</div>
    </div>}
  </div>;
}
