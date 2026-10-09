/**
 * Session bootstrap + React context (ARCHITECTURE.md §1 "Modes", §7 auth API).
 *
 * On load GET /api/auth/me decides the flow:
 *  - 200                          → signed in
 *  - 401 "no_sandbox"             → demo mode, show the demo landing page
 *  - 401 "unauthenticated" + demo → persona picker
 *  - 401 + production             → login form (401 "mfa_required" → TOTP code)
 */

import { createContext, useCallback, useContext, useEffect, useState } from 'react';
import type { FormEvent, ReactNode } from 'react';
import { api, ApiError } from './api';
import { Button } from './components/Button';
import { ErrorBanner } from './components/ErrorBanner';
import { TextField } from './components/fields';
import { Emblem, Icon, initials } from './components/icons';

export interface SessionUser {
  id: string;
  username: string;
  display_name: string;
  title: string;
  is_judge: boolean;
  persona: string | null;
  perms: string[];
}

/** Shape of GET /api/auth/me and POST /api/demo/login. */
export interface Session {
  user: SessionUser;
  mode: 'demo' | 'production';
  court_name: string;
  mfa_enrolled: boolean;
  must_change_password: boolean;
}

export interface Persona {
  key: string;
  display: string;
  title: string;
  summary: string;
}

type Stage =
  | { name: 'loading' }
  | { name: 'landing' }
  | { name: 'personas' }
  | { name: 'login' }
  | { name: 'totp' }
  | { name: 'enroll'; secret: string; otpauthUri: string }
  | { name: 'error'; error: unknown }
  | { name: 'ready'; session: Session };

export interface SessionContextValue {
  session: Session;
  /** True when the current user holds the permission string (server still enforces). */
  hasPerm: (perm: string) => boolean;
  /** Re-fetches /auth/me; resolves once the session is refreshed. */
  refresh: () => Promise<void>;
}

const SessionContext = createContext<SessionContextValue | null>(null);

/** Current session. Must be used inside <SessionProvider> (i.e. when signed in). */
export function useSession(): SessionContextValue {
  const ctx = useContext(SessionContext);
  if (!ctx) throw new Error('useSession must be used inside <SessionProvider>');
  return ctx;
}

export function SessionProvider({ children }: { children: ReactNode }) {
  const [stage, setStage] = useState<Stage>({ name: 'loading' });

  const refresh = useCallback(async () => {
    const me = await api<Session>('GET', '/auth/me');
    setStage({ name: 'ready', session: me });
  }, []);

  const boot = useCallback(async () => {
    try {
      await refresh();
      return;
    } catch (err) {
      if (!(err instanceof ApiError) || err.status !== 401) {
        setStage({ name: 'error', error: err });
        return;
      }
      if (err.code === 'no_sandbox') {
        setStage({ name: 'landing' });
        return;
      }
      try {
        const { mode } = await api<{ mode: 'demo' | 'production' }>('GET', '/auth/mode');
        if (mode === 'demo') setStage({ name: 'personas' });
        else setStage(err.code === 'mfa_required' ? { name: 'totp' } : { name: 'login' });
      } catch (e) {
        setStage({ name: 'error', error: e });
      }
    }
  }, [refresh]);

  useEffect(() => {
    void boot();
  }, [boot]);

  if (stage.name !== 'ready') {
    return <AuthGate stage={stage} go={setStage} refresh={refresh} retry={boot} />;
  }

  const session = stage.session;
  const hasPerm = (perm: string) => session.user.perms.includes(perm);
  return (
    <SessionContext.Provider value={{ session, hasPerm, refresh }}>
      {children}
    </SessionContext.Provider>
  );
}

/* ------------------------------------------------------------------ */
/* Unauthenticated gate screens                                        */
/* ------------------------------------------------------------------ */

function GateShell({ children }: { children: ReactNode }) {
  return (
    <div className="gate">
      <aside className="gate-hero">
        <div className="gate-brand">
          <Emblem size={42} />
          <span>Tuvalu Court Register</span>
        </div>
        <div className="gate-hero-body">
          <h1>Case management for the court registry</h1>
          <p>
            From the first filing to the final decision: registration, hearings, notices, decisions and a
            tamper-evident audit trail in one place.
          </p>
          <ul className="gate-points">
            <li><Icon name="inbox" /> Filings checked and registered as numbered cases</li>
            <li><Icon name="calendar" /> Hearings, rooms and notices to every participant</li>
            <li><Icon name="audit" /> Role-based access and a hash-chained audit log</li>
          </ul>
        </div>
        <p className="gate-foot">Independent prototype · fictional DEMO data</p>
      </aside>
      <main className="gate-panel" id="main">
        <div className="gate-card">{children}</div>
      </main>
    </div>
  );
}

function AuthGate({ stage, go, refresh, retry }: {
  stage: Stage;
  go: (s: Stage) => void;
  refresh: () => Promise<void>;
  retry: () => void;
}) {
  return (
    <GateShell>
      {stage.name === 'loading' && <p className="muted">Loading…</p>}
      {stage.name === 'landing' && <DemoLanding go={go} />}
      {stage.name === 'personas' && <PersonaPicker go={go} />}
      {stage.name === 'login' && <LoginForm go={go} refresh={refresh} />}
      {stage.name === 'totp' && <TotpForm refresh={refresh} />}
      {stage.name === 'enroll' && <EnrollForm secret={stage.secret} otpauthUri={stage.otpauthUri} refresh={refresh} />}
      {stage.name === 'error' && (
        <>
          <p>Could not start the session.</p>
          <ErrorBanner error={stage.error} onRetry={retry} />
        </>
      )}
    </GateShell>
  );
}

function DemoLanding({ go }: { go: (s: Stage) => void }) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const start = async () => {
    setBusy(true);
    setError(null);
    try {
      await api('POST', '/demo/start');
      go({ name: 'personas' });
    } catch (e) {
      setError(e);
    } finally {
      setBusy(false);
    }
  };
  return (
    <>
      <span className="gate-tag">DEMO · fictional data</span>
      <h2>Explore the demo</h2>
      <p className="gate-lead">
        This is an <strong>independent prototype</strong> of a court register, running on{' '}
        <strong>fictional DEMO data</strong>. Nothing you do here is sent to real people, and no real
        cases, names or filings exist in this system.
      </p>
      <p className="gate-lead">
        The demo runs in a private sandbox that belongs only to your browser. You can switch between
        staff personas and reset your own sandbox at any time.
      </p>
      <ErrorBanner error={error} onRetry={start} />
      <Button className="btn--lg" busy={busy} onClick={start}>Start the demo</Button>
    </>
  );
}

function PersonaPicker({ go }: { go: (s: Stage) => void }) {
  const [personas, setPersonas] = useState<Persona[] | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [busy, setBusy] = useState<string | null>(null);

  const load = useCallback(async () => {
    setError(null);
    try {
      setPersonas(await api<Persona[]>('GET', '/demo/personas'));
    } catch (e) {
      setError(e);
    }
  }, []);
  useEffect(() => {
    void load();
  }, [load]);

  const pick = async (key: string) => {
    setBusy(key);
    setError(null);
    try {
      const me = await api<Session>('POST', '/demo/login', { persona: key });
      go({ name: 'ready', session: me });
    } catch (e) {
      setError(e);
      setBusy(null);
    }
  };

  return (
    <>
      <span className="gate-tag">DEMO · fictional data</span>
      <h2>Choose who you are today</h2>
      <p className="gate-lead">Each persona has a different job and different permissions. You can switch at any time.</p>
      <ErrorBanner error={error} onRetry={load} />
      {!personas && !error && <p className="muted">Loading…</p>}
      <div className="persona-list">
        {personas?.map((p, i) => (
          <button
            key={p.key}
            type="button"
            className="persona-card"
            disabled={busy !== null}
            onClick={() => void pick(p.key)}
          >
            <span className="avatar avatar--lg" data-tone={i % 5} aria-hidden="true">{initials(p.display)}</span>
            <span className="persona-text">
              <strong>{p.display}</strong>
              <span className="persona-title">{p.title}</span>
              <span className="persona-summary">{p.summary}</span>
              {busy === p.key && <span className="muted">Signing in…</span>}
            </span>
            <Icon name="chevronRight" className="persona-go" />
          </button>
        ))}
      </div>
    </>
  );
}

function LoginForm({ go, refresh }: { go: (s: Stage) => void; refresh: () => Promise<void> }) {
  const [username, setUsername] = useState('');
  const [password, setPassword] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const res = await api<{ mfa_required: boolean; enroll_required: boolean }>(
        'POST', '/auth/login', { username, password },
      );
      if (res.enroll_required) {
        const setup = await api<{ secret: string; otpauth_uri: string }>('POST', '/auth/totp/setup');
        go({ name: 'enroll', secret: setup.secret, otpauthUri: setup.otpauth_uri });
      } else if (res.mfa_required) {
        go({ name: 'totp' });
      } else {
        await refresh();
      }
    } catch (err) {
      setError(err); // includes 429 "locked" — the server message is shown as-is
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit}>
      <h2>Sign in</h2>
      <p className="gate-lead">Sign in with your staff account.</p>
      <ErrorBanner error={error} />
      <TextField label="Username" value={username} onChange={setUsername} required autoComplete="username" />
      <TextField label="Password" type="password" value={password} onChange={setPassword} required autoComplete="current-password" />
      <Button type="submit" busy={busy}>Sign in</Button>
    </form>
  );
}

function TotpForm({ refresh }: { refresh: () => Promise<void> }) {
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api('POST', '/auth/totp', { code: code.trim() });
      await refresh();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit}>
      <h2>Authentication code</h2>
      <p className="muted">Enter the 6-digit code from your authenticator app.</p>
      <ErrorBanner error={error} />
      <TextField
        label="Code"
        value={code}
        onChange={setCode}
        required
        inputMode="numeric"
        autoComplete="one-time-code"
      />
      <Button type="submit" busy={busy}>Verify</Button>
    </form>
  );
}

function EnrollForm({ secret, otpauthUri, refresh }: {
  secret: string;
  otpauthUri: string;
  refresh: () => Promise<void>;
}) {
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);

  const submit = async (e: FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      await api('POST', '/auth/totp/enable', { code: code.trim() });
      await refresh();
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  };

  return (
    <form onSubmit={submit}>
      <h2>Set up your sign-in code</h2>
      <p>Add this key to an authenticator app (e.g. a TOTP app), then enter the code it shows.</p>
      <p><code className="totp-secret">{secret}</code></p>
      <p className="muted totp-uri">{otpauthUri}</p>
      <ErrorBanner error={error} />
      <TextField
        label="Code from the app"
        value={code}
        onChange={setCode}
        required
        inputMode="numeric"
        autoComplete="one-time-code"
      />
      <Button type="submit" busy={busy}>Enable and sign in</Button>
    </form>
  );
}
