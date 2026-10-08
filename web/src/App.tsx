/**
 * Application shell: top bar, demo banner, permission-filtered left navigation
 * and all routes. Pages live in src/pages/ — other agents fill in the stubs.
 */

import { useRef, useState } from 'react';
import { Link, NavLink, Route, Routes } from 'react-router-dom';
import { api } from './api';
import { SessionProvider, useSession } from './session';
import type { Persona } from './session';
import { Button } from './components/Button';
import { Modal } from './components/Modal';
import { PageHeader } from './components/PageHeader';
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
import GlobalSearch from './components/GlobalSearch';

const NAV: { to: string; label: string; end?: boolean; anyPerm?: string[] }[] = [
  { to: '/', label: 'Work queue', end: true },
  { to: '/intakes', label: 'Incoming' },
  { to: '/cases', label: 'Cases' },
  { to: '/calendar', label: 'Calendar' },
  { to: '/documents', label: 'Documents' },
  { to: '/decisions', label: 'Decisions' },
  { to: '/dispatch', label: 'Dispatch' },
  { to: '/mailbox', label: 'Mailbox' },
  { to: '/reports', label: 'Reports', anyPerm: ['report.view'] },
  { to: '/import', label: 'Import', anyPerm: ['import.run'] },
  { to: '/audit', label: 'Audit', anyPerm: ['audit.view'] },
  { to: '/settings', label: 'Settings', anyPerm: ['admin.users', 'admin.settings'] },
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
  return (
    <div className="shell">
      <a className="skip-link" href="#main">Skip to content</a>
      <TopBar />
      {session.mode === 'demo' && (
        <div className="demo-banner" role="note">
          DEMO — independent prototype, fictional data. Nothing is sent to real people.
        </div>
      )}
      <div className="shell-body">
        <SideNav />
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
      <Link to="/" className="brand">{session.court_name}</Link>
      <GlobalSearch />
      <div className="topbar-right">
        <span className="user-block">
          <span className="user-name">{session.user.display_name}</span>
          <span className="user-title">{session.user.title}</span>
        </span>
        {session.mode === 'demo' && <DemoControls />}
        <LogoutButton />
      </div>
    </header>
  );
}

function DemoControls() {
  const menuRef = useRef<HTMLDetailsElement>(null);
  const [personas, setPersonas] = useState<Persona[] | null>(null);
  const [personaError, setPersonaError] = useState(false);
  const [resetOpen, setResetOpen] = useState(false);
  const [busy, setBusy] = useState(false);

  const loadPersonas = async () => {
    if (personas) return;
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

  return (
    <>
      <details
        className="menu"
        ref={menuRef}
        onToggle={(e) => {
          if ((e.target as HTMLDetailsElement).open) void loadPersonas();
        }}
      >
        <summary className="menu-toggle">Switch person</summary>
        <div className="menu-list" role="menu">
          {personaError && <p className="menu-note">Could not load personas.</p>}
          {!personas && !personaError && <p className="menu-note">Loading…</p>}
          {personas?.map((p) => (
            <button key={p.key} type="button" role="menuitem" onClick={() => void switchPersona(p.key)}>
              <strong>{p.display}</strong>
              <span className="menu-note">{p.title}</span>
            </button>
          ))}
        </div>
      </details>

      <Button variant="secondary" onClick={() => setResetOpen(true)}>Reset my demo</Button>
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
  const { hasPerm } = useSession();
  return (
    <nav className="sidenav" aria-label="Main navigation">
      <ul>
        {NAV.filter((i) => !i.anyPerm || i.anyPerm.some(hasPerm)).map((i) => (
          <li key={i.to}>
            <NavLink to={i.to} end={i.end} className={({ isActive }) => (isActive ? 'active' : undefined)}>
              {i.label}
            </NavLink>
          </li>
        ))}
      </ul>
    </nav>
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
