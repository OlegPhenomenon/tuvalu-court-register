/**
 * Court time (ARCHITECTURE.md §3): Pacific/Funafuti is fixed UTC+12 with no
 * DST, so court-local values are computed by shifting the UTC instant by 12 h
 * and reading getUTC* — the browser's timezone can never move a court date.
 */

const OFFSET_MS = 12 * 60 * 60 * 1000;
const DAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'] as const;
const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'] as const;

const pad = (n: number) => String(n).padStart(2, '0');

/** UTC instant ("2026-11-16T21:00:00Z") → court-local "Tue 17 Nov 2026, 09:00". */
export function fmtLocal(utc: string): string {
  const d = new Date(new Date(utc).getTime() + OFFSET_MS);
  if (Number.isNaN(d.getTime())) return '—';
  return `${DAYS[d.getUTCDay()]} ${d.getUTCDate()} ${MONTHS[d.getUTCMonth()]} ${d.getUTCFullYear()}, ${pad(d.getUTCHours())}:${pad(d.getUTCMinutes())}`;
}

/**
 * Court-local wall-clock string ("2026-11-19T09:00") → "Thu 19 Nov 2026, 09:00".
 * `*_local` API fields carry no zone — this is pure string parsing; passing them
 * through `new Date` would shift them by the visitor's timezone.
 */
export function fmtCourtLocal(local: string): string {
  const [datePart, timePart] = local.split('T');
  const [y, m, d] = (datePart ?? '').split('-').map(Number);
  const time = (timePart ?? '').slice(0, 5);
  if (!y || !m || !d || m < 1 || m > 12 || d < 1 || d > 31 || !/^\d{2}:\d{2}$/.test(time)) {
    return local;
  }
  const wday = DAYS[new Date(Date.UTC(y, m - 1, d)).getUTCDay()];
  return `${wday} ${d} ${MONTHS[m - 1]} ${y}, ${time}`;
}

/** Month-year label for headings: "2026-11" or "2026-11-17" → "Nov 2026". */
export function fmtMonthYear(date: string): string {
  const y = Number(date.slice(0, 4));
  const m = Number(date.slice(5, 7));
  if (!y || !m || m < 1 || m > 12) return date;
  return `${MONTHS[m - 1]} ${y}`;
}

/** Court-local calendar date ("2026-11-17") → "17 Nov 2026". Pure string math. */
export function fmtDate(date: string): string {
  const [y, m, d] = date.split('-').map(Number);
  if (!y || !m || !d || m < 1 || m > 12) return date;
  return `${d} ${MONTHS[m - 1]} ${y}`;
}

/** Today's date in the court timezone, "YYYY-MM-DD". */
export function courtToday(): string {
  const d = new Date(Date.now() + OFFSET_MS);
  return `${d.getUTCFullYear()}-${pad(d.getUTCMonth() + 1)}-${pad(d.getUTCDate())}`;
}
