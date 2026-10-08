import { useState } from 'react';
import { api } from '../api';
import { useSession } from '../session';
import { Button } from '../components/Button';
import { ErrorBanner } from '../components/ErrorBanner';
import { TextField } from '../components/fields';

export default function ChangePassword({ forced = false }: { forced?: boolean }) {
  const { session, refresh } = useSession();
  const [current, setCurrent] = useState('');
  const [next, setNext] = useState('');
  const [confirm, setConfirm] = useState('');
  const [code, setCode] = useState('');
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [saved, setSaved] = useState(false);
  const submit = async () => {
    setError(null); setSaved(false);
    if (next !== confirm) { setError(new Error('New passwords do not match.')); return; }
    setBusy(true);
    try {
      await api('POST', '/auth/password', { current, new: next, code: code.trim() || undefined });
      setCurrent(''); setNext(''); setConfirm(''); setCode(''); setSaved(true);
      await refresh();
    } catch (e) { setError(e); } finally { setBusy(false); }
  };
  return <section aria-labelledby="password-title">
    <h2 id="password-title">{forced ? 'Change your temporary password' : 'Change my password'}</h2>
    <p>{forced ? 'Choose your own password before opening court records. ' : ''}Other sessions will end. Your sign-in code stays enabled.</p>
    <ErrorBanner error={error} />
    <form onSubmit={e => { e.preventDefault(); void submit(); }}>
      <fieldset className="admin-fieldset" disabled={busy}>
        <TextField label="Current password" type="password" value={current} onChange={setCurrent} autoComplete="current-password" required />
        <TextField label="New password" type="password" value={next} onChange={setNext} autoComplete="new-password" minLength={12} help="At least 12 characters. Choose a different password." required />
        <TextField label="Confirm new password" type="password" value={confirm} onChange={setConfirm} autoComplete="new-password" minLength={12} required />
        {session.mfa_enrolled && <TextField label="Fresh sign-in code" value={code} onChange={setCode} inputMode="numeric" autoComplete="one-time-code" required help="Wait for a new code if you just signed in." />}
        <Button type="submit" busy={busy}>Change password</Button>
      </fieldset>
    </form>
    {saved && <p role="status">Password changed. Other sessions have ended.</p>}
  </section>;
}
