/**
 * GET /api/ref — reference lists for forms (categories, channels, islands,
 * dispatch methods, closure bases, relation kinds, roles, rooms, registries,
 * staff, templates). Fetched once per session and cached in module memory —
 * never in localStorage, consistent with the no-persistent-case-data rule.
 */

import { useCallback, useEffect, useState } from 'react';
import { api } from '../api';
import { useSession } from '../session';
import type { Session } from '../session';

export interface RefItem {
  code: string;
  label: string;
}
export interface StaffUser {
  id: number;
  display_name: string;
  title: string | null;
  is_judge: number | boolean;
  /** False for system administrators who hold no case-work permission — never offer them for assignment. */
  assignable?: boolean;
}
export interface Registry {
  id: number;
  series: string;
  name: string;
}
export interface Room {
  id: number;
  name: string;
  location: string | null;
}
export interface MessageTemplate {
  code: string;
  name: string;
}

export interface RefData {
  /** kind → ordered items, e.g. lists.case_category, lists.intake_channel. */
  lists: Record<string, RefItem[]>;
  rooms: Room[];
  registries: Registry[];
  staff: StaffUser[];
  templates: MessageTemplate[];
  court_name: string;
  court_timezone: string;
  /** Court-local today, "YYYY-MM-DD". */
  today: string;
}

// A refreshed session (including a persona switch or sandbox reset) gets its own cache.
const caches = new WeakMap<Session, { data: RefData | null; pending: Promise<RefData> | null }>();
function sessionCache(session: Session) {
  let cache = caches.get(session);
  if (!cache) {
    cache = { data: null, pending: null };
    caches.set(session, cache);
  }
  return cache;
}
function fetchRef(cache: ReturnType<typeof sessionCache>): Promise<RefData> {
  if (cache.data) return Promise.resolve(cache.data);
  cache.pending ??= api<RefData>('GET', '/ref').then(
    (data) => { cache.data = data; cache.pending = null; return data; },
    (error) => { cache.pending = null; throw error; },
  );
  return cache.pending;
}

export interface UseRefResult {
  data: RefData | null;
  error: unknown;
  reload: () => void;
}

/**
 * Reference data for the whole session: `const { data: ref, error } = useRef()`.
 * (Named `useRef` by contract; alias the import — `useRef as useRefData` — in
 * files that also need React's `useRef`.)
 */
export function useRef(): UseRefResult {
  const { session } = useSession();
  const cache = sessionCache(session);
  const [data, setData] = useState<RefData | null>(cache.data);
  const [error, setError] = useState<unknown>(null);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    let alive = true;
    setData(cache.data);
    setError(null);
    fetchRef(cache).then(
      (d) => {
        if (alive) setData(d);
      },
      (e) => {
        if (alive) setError(e);
      },
    );
    return () => {
      alive = false;
    };
  }, [cache, tick]);

  const reload = useCallback(() => {
    cache.data = null;
    setError(null);
    setTick((t) => t + 1);
  }, [cache]);

  return { data: data === cache.data ? data : cache.data, error, reload };
}

/** One reference list by kind; empty array when ref data is not loaded yet. */
export function refList(data: RefData | null | undefined, kind: string): RefItem[] {
  return data?.lists[kind] ?? [];
}

/** Human label of a code in a ref list; falls back to the raw code. */
export function label(list: RefItem[] | undefined | null, code: string | null | undefined): string {
  if (!code) return '—';
  return list?.find((i) => i.code === code)?.label ?? code;
}

/** A ref list as SelectField `options`. */
export function options(list: RefItem[] | undefined | null): { value: string; label: string }[] {
  return (list ?? []).map((i) => ({ value: i.code, label: i.label }));
}

/** Staff list as SelectField options ("Name — title"). */
export function staffOptions(staff: StaffUser[] | undefined | null): { value: string; label: string }[] {
  return (staff ?? []).map((s) => ({
    value: String(s.id),
    label: s.title ? `${s.display_name} — ${s.title}` : s.display_name,
  }));
}
