/**
 * Small stroke icons drawn inline (no icon font, no external assets).
 * `<Icon name="cases" />` — decorative by default (aria-hidden).
 */

const PATHS = {
  queue: 'M4 6h16M4 12h10M4 18h7M17 15l2 2 3-4',
  inbox: 'M3 13h5l2 3h4l2-3h5M5.5 5h13L21 13v6a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1v-6z',
  cases: 'M3 7a1 1 0 0 1 1-1h5l2 2h9a1 1 0 0 1 1 1v10a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1zM3 11h18',
  calendar: 'M4 6a1 1 0 0 1 1-1h14a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1H5a1 1 0 0 1-1-1zM4 10h16M8 3v4M16 3v4M8 14h2M14 14h2M8 17h2',
  documents: 'M14 3H7a1 1 0 0 0-1 1v16a1 1 0 0 0 1 1h10a1 1 0 0 0 1-1V7zM14 3v4h4M9 12h6M9 16h6',
  decisions: 'M14 5l5 5M11.5 7.5l5 5M9 16l6.5-6.5M4 20h9M13 3.5l7.5 7.5-2.5 2.5L10.5 6z',
  dispatch: 'M21 3L10 14M21 3l-7 18-4-7-7-4z',
  mailbox: 'M3 6a1 1 0 0 1 1-1h16a1 1 0 0 1 1 1v12a1 1 0 0 1-1 1H4a1 1 0 0 1-1-1zM3 7l9 6 9-6',
  reports: 'M4 20V10M10 20V4M16 20v-7M22 20H2',
  import: 'M12 3v12M7 10l5 5 5-5M4 17v2a1 1 0 0 0 1 1h14a1 1 0 0 0 1-1v-2',
  audit: 'M12 3l8 3v6c0 4.5-3.4 8-8 9-4.6-1-8-4.5-8-9V6zM8.5 12l2.5 2.5 4.5-5',
  settings: 'M4 7h10M18 7h2M4 17h4M12 17h8M14 4v6M8 14v6',
  search: 'M11 4a7 7 0 1 0 0 14 7 7 0 0 0 0-14zM20 20l-4-4',
  chevronDown: 'M6 9l6 6 6-6',
  chevronRight: 'M9 6l6 6-6 6',
  logout: 'M15 4h3a1 1 0 0 1 1 1v14a1 1 0 0 1-1 1h-3M10 16l-4-4 4-4M6 12h10',
  reset: 'M4 12a8 8 0 1 0 2.3-5.6M4 4v4h4',
  users: 'M9 11a4 4 0 1 0 0-8 4 4 0 0 0 0 8zM2 21v-1a6 6 0 0 1 12 0v1M16 3.5a4 4 0 0 1 0 7M22 21v-1a6 6 0 0 0-4-5.6',
  clock: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 7v5l3 2',
  arrowRight: 'M5 12h14M13 6l6 6-6 6',
  info: 'M12 3a9 9 0 1 0 0 18 9 9 0 0 0 0-18zM12 11v5M12 8h.01',
  check: 'M5 12l5 5 9-10',
} as const;

export type IconName = keyof typeof PATHS;

export function Icon({ name, size = 18, className }: { name: IconName; size?: number; className?: string }) {
  return (
    <svg
      className={className ? `icon ${className}` : 'icon'}
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.75}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
      focusable="false"
    >
      <path d={PATHS[name]} />
    </svg>
  );
}

/** Court emblem: a shield with balance scales, used by the sidebar and sign-in screen. */
export function Emblem({ size = 36 }: { size?: number }) {
  return (
    <svg className="emblem" width={size} height={size} viewBox="0 0 40 40" aria-hidden="true" focusable="false">
      <path d="M20 2.5 34.5 8v11c0 9.2-6.2 16.3-14.5 18.5C11.7 35.3 5.5 28.2 5.5 19V8z" fill="var(--emblem-bg, #13315f)" stroke="var(--emblem-ring, #f2c14e)" strokeWidth="1.6" />
      <g fill="none" stroke="var(--emblem-ink, #f2c14e)" strokeWidth="1.6" strokeLinecap="round" strokeLinejoin="round">
        <path d="M20 10.5v17M14 30h12M12.5 14h15" />
        <path d="M12.5 14l-3.2 7.2h6.4zM27.5 14l-3.2 7.2h6.4z" />
      </g>
      <circle cx="20" cy="10" r="1.6" fill="var(--emblem-ink, #f2c14e)" />
    </svg>
  );
}

/** Two-letter initials for an avatar circle. */
export function initials(name: string): string {
  const parts = name.replace(/\(.*?\)/g, '').trim().split(/\s+/).filter(Boolean);
  return ((parts[0]?.[0] ?? '') + (parts.length > 1 ? parts[parts.length - 1]![0] : '')).toUpperCase() || '?';
}
