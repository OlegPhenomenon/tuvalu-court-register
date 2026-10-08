/**
 * Court calendar (§8): day / week / month views over GET /api/hearings, with
 * judge and room filters. All dates are court-local (Pacific/Funafuti) — day
 * arithmetic is pure string/UTC-midnight math so the browser timezone can never
 * move a court date. Adjourned and cancelled hearings stay visible, struck
 * through. Printing yields a daily list marked "For internal use — not for
 * publication" (calendar.css carries the print rules).
 */

import { useMemo, useState } from 'react';
import { Link } from 'react-router-dom';
import { useSession } from '../session';
import { courtToday, fmtDate } from '../time';
import { Button } from '../components/Button';
import { ErrorBanner } from '../components/ErrorBanner';
import { SelectField } from '../components/fields';
import { PageHeader } from '../components/PageHeader';
import { StatusBadge } from '../components/StatusBadge';
import { useRef as useRefData } from '../components/refdata';
import { useApi } from '../components/useApi';
import { hearingTimeRange } from '../components/HearingForm';
import type { Hearing } from '../components/HearingForm';
import './calendar.css';

type View = 'day' | 'week' | 'month';

const DAY_MS = 24 * 60 * 60 * 1000;
const WDAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'] as const;
const WDAYS_FULL = ['Sunday', 'Monday', 'Tuesday', 'Wednesday', 'Thursday', 'Friday', 'Saturday'] as const;
const WEEK_HEADER = ['Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat', 'Sun'] as const;

const pad = (n: number) => String(n).padStart(2, '0');
const parseDay = (d: string) => Date.parse(`${d}T00:00:00Z`);
const dayStr = (ms: number) => new Date(ms).toISOString().slice(0, 10);
const addDays = (d: string, n: number) => dayStr(parseDay(d) + n * DAY_MS);
const weekday = (d: string) => new Date(parseDay(d)).getUTCDay(); // 0 = Sunday
const mondayOf = (d: string) => addDays(d, -((weekday(d) + 6) % 7));
const dayNum = (d: string) => Number(d.slice(8, 10));

/** Same day-of-month one month earlier/later, clamped to the month's length. */
function addMonth(d: string, n: number): string {
  const y = Number(d.slice(0, 4));
  const m = Number(d.slice(5, 7));
  const day = Number(d.slice(8, 10));
  const total = y * 12 + (m - 1) + n;
  const ny = Math.floor(total / 12);
  const nm = (total % 12) + 1;
  const dim = new Date(Date.UTC(ny, nm, 0)).getUTCDate();
  return `${ny}-${pad(nm)}-${pad(Math.min(day, dim))}`;
}

/** Inclusive date range covering the view (month covers its whole grid). */
function viewRange(view: View, date: string): { from: string; to: string; days: string[] } {
  if (view === 'day') return { from: date, to: date, days: [date] };
  if (view === 'week') {
    const start = mondayOf(date);
    const days = Array.from({ length: 7 }, (_, i) => addDays(start, i));
    return { from: start, to: days[6]!, days };
  }
  const first = `${date.slice(0, 8)}01`;
  const start = mondayOf(first);
  const last = dayStr(parseDay(addMonth(first, 1)) - DAY_MS);
  const days: string[] = [];
  for (let d = start; ; d = addDays(d, 1)) {
    days.push(d);
    if (d >= last && weekday(d) === 0) break; // stop on the Sunday covering the last day
  }
  return { from: start, to: days[days.length - 1]!, days };
}

/** Badge-tone colour of a hearing status for calendar chips. */
function tone(status: string): string {
  switch (status) {
    case 'scheduled':
      return 'info';
    case 'held':
      return 'success';
    case 'adjourned':
      return 'warn';
    default:
      return 'neutral';
  }
}

function Chip({ h }: { h: Hearing }) {
  const dead = h.status === 'adjourned' || h.status === 'cancelled';
  return (
    <Link
      className={`cal-chip cal-chip--${tone(h.status)}${dead ? ' cal-chip--dead' : ''}`}
      to={`/cases/${h.case_id}?tab=hearings`}
      title={`${h.hearing_type_label} — ${hearingTimeRange(h)}${h.judge_name ? `, ${h.judge_name}` : ''}`}
    >
      <span className="cal-chip-time">{h.starts_local.slice(11, 16)}</span>{' '}
      <span className="cal-chip-case">{h.case_number}</span>
      {h.room_name && <span className="cal-chip-room"> {h.room_name}</span>}
    </Link>
  );
}

export default function Calendar() {
  const { session } = useSession();
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const [view, setView] = useState<View>('day');
  const [date, setDate] = useState(courtToday());
  const [judge, setJudge] = useState('');
  const [room, setRoom] = useState('');

  const { from, to, days } = viewRange(view, date);
  const path =
    `/hearings?from=${from}&to=${to}` +
    (judge ? `&judge=${encodeURIComponent(judge)}` : '') +
    (room ? `&room=${encodeURIComponent(room)}` : '');
  const { data, error, loading, reload } = useApi<{ items: Hearing[] }>(path);

  const items = data?.items ?? [];
  const byDay = useMemo(() => {
    const map = new Map<string, Hearing[]>();
    for (const h of items) {
      const day = h.starts_local.slice(0, 10);
      const list = map.get(day) ?? [];
      list.push(h);
      map.set(day, list);
    }
    return map;
  }, [items]);

  const today = courtToday();
  const step = (n: number) =>
    setDate(view === 'day' ? addDays(date, n) : view === 'week' ? addDays(date, 7 * n) : addMonth(date, n));

  const rangeLabel =
    view === 'day'
      ? `${WDAYS_FULL[weekday(date)]}, ${fmtDate(date)}`
      : view === 'week'
        ? `Week of ${fmtDate(from)}`
        : `${fmtDate(`${date.slice(0, 8)}01`).slice(3)}`; // "Nov 2026"

  const judges = (ref?.staff ?? []).filter((s) => s.is_judge);
  const printItems = byDay.get(date) ?? [];

  return (
    <>
      <PageHeader title="Calendar" />
      <ErrorBanner error={refError} onRetry={reloadRef} />

      <div className="cal-screen">
        <div className="cal-toolbar">
          <div className="cal-nav" role="group" aria-label="Change the period">
            <Button variant="secondary" onClick={() => step(-1)} aria-label="Previous period">‹</Button>
            <Button variant="secondary" onClick={() => setDate(courtToday())}>Today</Button>
            <Button variant="secondary" onClick={() => step(1)} aria-label="Next period">›</Button>
          </div>
          <div className="cal-views" role="group" aria-label="Calendar view">
            {(['day', 'week', 'month'] as const).map((v) => (
              <Button
                key={v}
                variant={view === v ? 'primary' : 'secondary'}
                aria-pressed={view === v}
                onClick={() => setView(v)}
              >
                {v[0]!.toUpperCase() + v.slice(1)}
              </Button>
            ))}
          </div>
          <SelectField
            label="Judge"
            value={judge}
            onChange={setJudge}
            options={judges.map((j) => ({ value: String(j.id), label: j.display_name }))}
            placeholder="All judges"
          />
          <SelectField
            label="Room"
            value={room}
            onChange={setRoom}
            options={(ref?.rooms ?? []).map((r) => ({
              value: String(r.id),
              label: r.location ? `${r.name} — ${r.location}` : r.name,
            }))}
            placeholder="All rooms"
          />
        </div>

        <p className="cal-range">{rangeLabel}</p>
        <ErrorBanner error={error} onRetry={reload} />
        {loading && <p className="muted">Loading…</p>}
        {!loading && !error && items.length === 0 && (
          <p className="muted">No hearings in this period{judge || room ? ' for the chosen filters' : ''}.</p>
        )}
        <p className="muted cal-legend">
          Struck through = adjourned or cancelled; both stay on the record. Times are court time
          (Pacific/Funafuti).
        </p>

        {view === 'day' && items.length > 0 && (
          <div className="table-wrap">
            <table>
              <thead>
                <tr>
                  <th scope="col">Time</th>
                  <th scope="col">Case</th>
                  <th scope="col">Type</th>
                  <th scope="col">Room</th>
                  <th scope="col">Judge</th>
                  <th scope="col">Status</th>
                </tr>
              </thead>
              <tbody>
                {(byDay.get(date) ?? []).map((h) => {
                  const dead = h.status === 'adjourned' || h.status === 'cancelled';
                  return (
                    <tr key={h.id} className={dead ? 'cal-row--dead' : undefined}>
                      <td>
                        {h.starts_local.slice(11, 16)} – {h.ends_local.slice(11, 16)}
                      </td>
                      <td>
                        <Link to={`/cases/${h.case_id}?tab=hearings`}>{h.case_number}</Link>
                      </td>
                      <td>{h.hearing_type_label}</td>
                      <td>{h.room_name ?? '—'}</td>
                      <td>{h.judge_name ?? '—'}</td>
                      <td><StatusBadge status={h.status} /></td>
                    </tr>
                  );
                })}
              </tbody>
            </table>
          </div>
        )}

        {view === 'week' && (
          <div className="cal-grid cal-week" role="list">
            {days.map((d) => (
              <section
                key={d}
                role="listitem"
                className={`cal-day${d === today ? ' cal-day--today' : ''}`}
                aria-label={`${WDAYS_FULL[weekday(d)]} ${fmtDate(d)}`}
              >
                <h3 className="cal-day-head">
                  {WDAYS[weekday(d)]} <span className="cal-daynum">{fmtDate(d)}</span>
                </h3>
                {(byDay.get(d) ?? []).map((h) => <Chip key={h.id} h={h} />)}
              </section>
            ))}
          </div>
        )}

        {view === 'month' && (
          <>
            <div className="cal-weekdays" aria-hidden="true">
              {WEEK_HEADER.map((w) => <span key={w}>{w}</span>)}
            </div>
            <div className="cal-grid cal-month-grid">
              {days.map((d) => (
                <div
                  key={d}
                  className={`cal-cell${d.slice(0, 7) !== date.slice(0, 7) ? ' cal-cell--other' : ''}${d === today ? ' cal-day--today' : ''}`}
                >
                  <span className="cal-daynum">{dayNum(d)}</span>
                  {(byDay.get(d) ?? []).map((h) => <Chip key={h.id} h={h} />)}
                </div>
              ))}
            </div>
          </>
        )}
      </div>

      {/* Print output: the daily list for the selected day (calendar.css switches to it). */}
      <div className="cal-print" aria-hidden="true">
        <h1 className="cal-print-title">
          {session.court_name} — hearings on {fmtDate(date)}
        </h1>
        <p className="cal-print-note">For internal use — not for publication.</p>
        {printItems.length === 0 ? (
          <p>No hearings on this day.</p>
        ) : (
          <table>
            <thead>
              <tr>
                <th scope="col">Time</th>
                <th scope="col">Case</th>
                <th scope="col">Type</th>
                <th scope="col">Room</th>
                <th scope="col">Judge</th>
                <th scope="col">Status</th>
              </tr>
            </thead>
            <tbody>
              {printItems.map((h) => (
                <tr key={h.id}>
                  <td>
                    {h.starts_local.slice(11, 16)} – {h.ends_local.slice(11, 16)}
                  </td>
                  <td>{h.case_number}</td>
                  <td>{h.hearing_type_label}</td>
                  <td>{h.room_name ?? '—'}</td>
                  <td>{h.judge_name ?? '—'}</td>
                  <td>{h.status}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        <p className="cal-print-note">
          Printed {fmtDate(today)} — struck-through or cancelled entries stay on the record.
        </p>
      </div>
    </>
  );
}
