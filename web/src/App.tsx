/**
 * Application shell: dark sidebar (brand, grouped permission-filtered
 * navigation, DEMO note), top bar (search, user menu) and all routes.
 * Pages live in src/pages/.
 */

import { useEffect, useRef, useState } from 'react';
import { Link, NavLink, Route, Routes } from 'react-router-dom';
import { api } from './api';
import { isAdminOnly, SessionProvider, useSession } from './session';
import type { Persona } from './session';
import { Button } from './components/Button';
import { Modal } from './components/Modal';
import { PageHeader } from './components/PageHeader';
import { Emblem, Icon, initials } from './components/icons';
import type { IconName } from './components/icons';
import WorkQueue from './pages/WorkQueue';
import Intakes from './pages/Intakes';
import IntakeDetail from './pages/IntakeDetail';
import Cases from './pages/Cases';
import CaseWorkspace from './pages/CaseWorkspace';
import Calendar from './pages/Calendar';
import Documents from './pages/Documents';
import Decisions from './pages/Decisions';
import Dispatch from './pages/Dispatch';
import Mailbox from './pages/Mailbox';
import Reports from './pages/Reports';
import Import from './pages/Import';
import Audit from './pages/Audit';
import Settings from './pages/Settings';
import ChangePassword from './pages/ChangePassword';
import GlobalSearch from './components/GlobalSearch';

/** `caseWork` items need case access, which admin-only roles never have. */
type NavItem = { to: string; label: string; icon: IconName; end?: boolean; anyPerm?: string[]; caseWork?: boolean };

const NAV: { title: string; items: NavItem[] }[] = [
  {
    title: 'Workspace',
    items: [
      { to: '/', label: 'Work queue', icon: 'queue', end: true },
      { to: '/intakes', label: 'Incoming', icon: 'inbox', caseWork: true },
      { to: '/cases', label: 'Cases', icon: 'cases', caseWork: true },
      { to: '/calendar', label: 'Calendar', icon: 'calendar', caseWork: true },
    ],
  },
  {
    title: 'Records',
    items: [
      { to: '/documents', label: 'Documents', icon: 'documents', caseWork: true },
      { to: '/decisions', label: 'Decisions', icon: 'decisions', caseWork: true },
      { to: '/dispatch', label: 'Dispatch', icon: 'dispatch', caseWork: true },
      { to: '/mailbox', label: 'Mailbox', icon: 'mailbox', caseWork: true },
    ],
  },
  {
    title: 'Administration',
    items: [
      { to: '/reports', label: 'Reports', icon: 'reports', anyPerm: ['report.view'] },
      { to: '/import', label: 'Import', icon: 'import', anyPerm: ['import.run'] },
      { to: '/audit', label: 'Audit', icon: 'audit', anyPerm: ['audit.view'] },
      { to: '/settings', label: 'Settings', icon: 'settings' },
    ],
  },
];

export default function App() {
  return (
    <SessionProvider>
      <Shell />
    </SessionProvider>
  );
}

function Shell() {
  const { session } = useSession();
  if (session.must_change_password) return <div className="gate gate--single"><main className="gate-card" id="main"><h1>Tuvalu Court Register</h1><ChangePassword forced /><LogoutButton /></main></div>;
  return (
    <div className="shell">
      <a className="skip-link" href="#main">Skip to content</a>
      <SideNav />
      <div className="shell-main">
        <TopBar />
        <main className="main" id="main" tabIndex={-1}>
          <Routes>
            <Route path="/" element={<WorkQueue />} />
            <Route path="/intakes" element={<Intakes />} />
            <Route path="/intakes/:id" element={<IntakeDetail />} />
            <Route path="/cases" element={<Cases />} />
            <Route path="/cases/:id" element={<CaseWorkspace />} />
            <Route path="/calendar" element={<Calendar />} />
            <Route path="/documents" element={<Documents />} />
            <Route path="/decisions" element={<Decisions />} />
            <Route path="/dispatch" element={<Dispatch />} />
            <Route path="/mailbox" element={<Mailbox />} />
            <Route path="/reports" element={<Reports />} />
            <Route path="/import" element={<Import />} />
            <Route path="/audit" element={<Audit />} />
            <Route path="/settings" element={<Settings />} />
            <Route path="*" element={<NotFound />} />
          </Routes>
        </main>
      </div>
    </div>
  );
}

function TopBar() {
  const { session } = useSession();
  return (
    <header className="topbar">
      <GlobalSearch />
      <div className="topbar-right">
        {session.mode === 'demo' && (
          <span className="env-pill" title="Independent prototype on fictional data. Nothing is sent to real people.">
            DEMO environment
          </span>
        )}
        <UserMenu />
      </div>
    </header>
  );
}

function UserMenu() {
  const { session } = useSession();
  const demo = session.mode === 'demo';
  const menuRef = useRef<HTMLDetailsElement>(null);
  const [personas, setPersonas] = useState<Persona[] | null>(null);
  const [personaError, setPersonaError] = useState(false);
  const [resetOpen, setResetOpen] = useState(false);
  const [busy, setBusy] = useState(false);

  // Close the menu on a click outside it or on Escape.
  useEffect(() => {
    const onPointer = (e: PointerEvent) => {
      const el = menuRef.current;
      if (el?.open && !el.contains(e.target as Node)) el.open = false;
    };
    const onKey = (e: KeyboardEvent) => {
      const el = menuRef.current;
      if (e.key === 'Escape' && el?.open) {
        el.open = false;
        el.querySelector('summary')?.focus();
      }
    };
    document.addEventListener('pointerdown', onPointer);
    document.addEventListener('keydown', onKey);
    return () => {
      document.removeEventListener('pointerdown', onPointer);
      document.removeEventListener('keydown', onKey);
    };
  }, []);

  const loadPersonas = async () => {
    if (personas || !demo) return;
    try {
      setPersonas(await api<Persona[]>('GET', '/demo/personas'));
    } catch {
      setPersonaError(true);
    }
  };

  const switchPersona = async (key: string) => {
    await api('POST', '/demo/login', { persona: key });
    // A full reload lands the new person on the work queue — no screen (and no
    // restricted data) of the previous persona stays visible.
    window.location.assign('/');
  };

  const reset = async () => {
    setBusy(true);
    try {
      await api('POST', '/demo/reset');
      window.location.assign('/');
    } finally {
      setBusy(false);
    }
  };

  const logout = async () => {
    try {
      await api('POST', '/auth/logout');
    } finally {
      // Full reload clears every piece of in-memory case data.
      window.location.reload();
    }
  };

  const closeMenu = () => {
    if (menuRef.current) menuRef.current.open = false;
  };

  return (
    <>
      <details
        className="menu user-menu"
        ref={menuRef}
        onToggle={(e) => {
          if ((e.target as HTMLDetailsElement).open) void loadPersonas();
        }}
      >
        <summary className="user-trigger" aria-label={`Account: ${session.user.display_name}`}>
          <span className="avatar" aria-hidden="true">{initials(session.user.display_name)}</span>
          <span className="user-block">
            <span className="user-name">{session.user.display_name}</span>
            <span className="user-title">{session.user.title}</span>
          </span>
          <Icon name="chevronDown" size={16} className="user-chevron" />
        </summary>
        <div className="menu-list" role="menu">
          {demo && (
            <>
              <p className="menu-heading">Switch person</p>
              {personaError && <p className="menu-note">Could not load personas.</p>}
              {!personas && !personaError && <p className="menu-note">Loading…</p>}
              {personas?.map((p) => {
                const current = p.key === session.user.persona;
                return (
                  <button
                    key={p.key}
                    type="button"
                    role="menuitem"
                    className={current ? 'menu-person is-current' : 'menu-person'}
                    aria-current={current || undefined}
                    onClick={() => void switchPersona(p.key)}
                  >
                    <span className="avatar avatar--sm" aria-hidden="true">{initials(p.display)}</span>
                    <span className="menu-person-text">
                      <strong>{p.display}</strong>
                      <span>{p.title}</span>
                    </span>
                    {current && <Icon name="check" size={16} className="menu-check" />}
                  </button>
                );
              })}
              <hr className="menu-sep" />
              <button
                type="button"
                role="menuitem"
                className="menu-item"
                onClick={() => {
                  closeMenu();
                  setResetOpen(true);
                }}
              >
                <Icon name="reset" size={16} /> Reset my demo
              </button>
            </>
          )}
          <button type="button" role="menuitem" className="menu-item" onClick={() => void logout()}>
            <Icon name="logout" size={16} /> Log out
          </button>
        </div>
      </details>

      {demo && (
        <Modal title="Reset my demo" open={resetOpen} onClose={() => setResetOpen(false)}>
          <p>
            This wipes <strong>your own sandbox</strong> and reloads it with fresh fictional data.
            Other visitors are not affected.
          </p>
          <div className="actions">
            <Button variant="danger" busy={busy} onClick={reset}>Reset my demo</Button>
            <Button variant="secondary" onClick={() => setResetOpen(false)}>Cancel</Button>
          </div>
        </Modal>
      )}
    </>
  );
}

function LogoutButton() {
  const [busy, setBusy] = useState(false);
  const logout = async () => {
    setBusy(true);
    try {
      await api('POST', '/auth/logout');
    } finally {
      // Full reload clears every piece of in-memory case data.
      window.location.reload();
    }
  };
  return <Button variant="secondary" busy={busy} onClick={logout}>Log out</Button>;
}

function SideNav() {
  const { session, hasPerm } = useSession();
  const adminOnly = isAdminOnly(session.user);
  return (
    <aside className="sidebar">
      <Link to="/" className="sidebar-brand">
        <Emblem size={34} />
        <span className="sidebar-brand-text">
          <strong>Tuvalu Court Register</strong>
          <small>{session.court_name}</small>
        </span>
      </Link>
      <nav className="sidenav" aria-label="Main navigation">
        {NAV.map((section) => {
          const items = section.items.filter((i) => (!i.anyPerm || i.anyPerm.some(hasPerm)) && !(i.caseWork && adminOnly));
          if (items.length === 0) return null;
          return (
            <div className="nav-section" key={section.title}>
              <p className="nav-heading">{section.title}</p>
              <ul>
                {items.map((i) => (
                  <li key={i.to}>
                    <NavLink to={i.to} end={i.end} className={({ isActive }) => (isActive ? 'active' : undefined)}>
                      <Icon name={i.icon} />
                      <span>{i.label}</span>
                    </NavLink>
                  </li>
                ))}
              </ul>
            </div>
          );
        })}
      </nav>
      {session.mode === 'demo' && (
        <div className="sidebar-note" role="note">
          <strong>DEMO</strong> — independent prototype, fictional data. Nothing is sent to real people.
        </div>
      )}
    </aside>
  );
}

function NotFound() {
  return (
    <>
      <PageHeader title="Page not found" />
      <p className="muted">This address does not exist in the register.</p>
    </>
  );
}
