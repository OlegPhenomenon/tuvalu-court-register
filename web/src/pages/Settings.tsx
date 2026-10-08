import { useState } from 'react';
import { api } from '../api';
import { useSession } from '../session';
import { fmtLocal } from '../time';
import { PageHeader } from '../components/PageHeader';
import { Card } from '../components/Card';
import { Button } from '../components/Button';
import { DataTable } from '../components/DataTable';
import { Modal } from '../components/Modal';
import { ConfirmReasonDialog } from '../components/ConfirmReasonDialog';
import { ErrorBanner } from '../components/ErrorBanner';
import { CheckboxField, SelectField, TextArea, TextField } from '../components/fields';
import { useApi } from '../components/useApi';
import { useRef as useRefData } from '../components/refdata';
import './admin.css';

type Permission = { key: string; description: string; admin_grantable: boolean };
type User = { id: number; username: string; display_name: string; title: string | null; email: string | null; is_judge: number | boolean; active: number | boolean; permissions: string[]; mfa_enrolled: number | boolean; active_sessions: number; last_seen_at: string | null };
type Secret = { temporary_password?: string | null; note?: string };
const CLI = 'This permission must be granted by the court authority on the server (tuvalu-court grant).';
function PermissionFields({ permissions, selected, setSelected, locked }: { permissions: Permission[]; selected: string[]; setSelected: (next: string[]) => void; locked?: boolean }) {
  return <fieldset className="admin-fieldset"><legend>Permissions</legend><p>{CLI}</p>{permissions.map(p => <CheckboxField key={p.key} label={`${p.key} — ${p.description}`} checked={selected.includes(p.key)} disabled={locked || !p.admin_grantable} help={!p.admin_grantable ? CLI : undefined} onChange={checked => setSelected(checked ? [...selected, p.key] : selected.filter(key => key !== p.key))} />)}</fieldset>;
}
function SecretDialog({ secret, onClose }: { secret: Secret; onClose: () => void }) {
  const [copied, setCopied] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const copy = async () => { try { await navigator.clipboard.writeText(secret.temporary_password ?? ''); setCopied(true); } catch { setError(new Error('Copy unavailable')); } };
  return <Modal title={secret.temporary_password ? 'One-time temporary password' : 'User created'} open onClose={onClose}>
    {secret.temporary_password ? <><p>Save this temporary password now and give it to the staff member. It is shown only once; after closing this dialog it cannot be retrieved. The user must change it at sign-in.</p><p className="admin-secret"><code>{secret.temporary_password}</code></p><Button variant="secondary" onClick={() => void copy()}>Copy password</Button>{copied && <p role="status">Copied.</p>}{error && <p role="alert">Could not copy. Select the password above and copy it manually.</p>}</> : <p>{secret.note ?? 'Demo: new people cannot sign in; use the persona switcher.'}</p>}
    <div className="actions"><Button onClick={onClose}>{secret.temporary_password ? 'I have saved it — close' : 'Close'}</Button></div>
  </Modal>;
}
function UserEditor({ user, mode, permissions, protectedAccount, onClose, onSaved, onSecret }: { user: User | null; mode: 'create' | 'edit' | 'permissions'; permissions: Permission[]; protectedAccount: boolean; onClose: () => void; onSaved: () => void; onSecret: (secret: Secret) => void }) {
  const [username, setUsername] = useState('');
  const [name, setName] = useState(user?.display_name ?? '');
  const [title, setTitle] = useState(user?.title ?? '');
  const [email, setEmail] = useState(user?.email ?? '');
  const [selected, setSelected] = useState(user?.permissions ?? []);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const submit = async () => {
    setBusy(true); setError(null);
    try {
      if (mode === 'create') { const result = await api<Secret>('POST', '/admin/users', { username, display_name: name, title, email, permissions: selected }); onClose(); onSecret(result); }
      else if (mode === 'permissions' && user) { await api('PUT', `/admin/users/${user.id}/permissions`, { permissions: selected }); onClose(); }
      else if (user) { await api('PATCH', `/admin/users/${user.id}`, { display_name: name, title, email }); onClose(); }
      onSaved();
    } catch (e) { setError(e); } finally { setBusy(false); }
  };
  return <Modal title={mode === 'create' ? 'Create user' : `${mode === 'edit' ? 'Edit user' : 'Permissions'}: ${user?.display_name}`} open onClose={busy ? () => {} : onClose}>
    <ErrorBanner error={error} onRetry={() => void submit()} />
    <form onSubmit={e => { e.preventDefault(); void submit(); }}><fieldset className="admin-fieldset" disabled={busy}>
      {mode === 'create' && <TextField label="Username" value={username} onChange={setUsername} required minLength={3} maxLength={32} pattern="[a-z0-9._\-]+" help="3–32 lowercase letters, digits, dots, underscores or hyphens." />}
      {mode !== 'permissions' && <><TextField label="Display name" value={name} onChange={setName} required /><TextField label="Job title" value={title} onChange={setTitle} /><TextField label="Email" type="email" value={email} onChange={setEmail} /></>}
      {mode !== 'edit' && <PermissionFields permissions={permissions} selected={selected} setSelected={setSelected} locked={protectedAccount} />}
      {mode === 'permissions' && protectedAccount && <p>This account holds court-authority powers. Its permissions must be managed through the court CLI.</p>}
      <div className="actions"><Button type="submit" busy={busy} disabled={mode === 'permissions' && protectedAccount}>{mode === 'create' ? 'Create user' : 'Save changes'}</Button><Button type="button" variant="secondary" onClick={onClose}>Cancel</Button></div>
    </fieldset></form>
  </Modal>;
}
type AccountAction = 'deactivate' | 'reactivate' | 'revoke-sessions' | 'reset-password' | 'reset-mfa';
const actionLabels: Record<AccountAction, string> = { deactivate: 'Deactivate', reactivate: 'Reactivate', 'revoke-sessions': 'Revoke sessions', 'reset-password': 'Reset password', 'reset-mfa': 'Reset sign-in code' };
function Users({ onChanged }: { onChanged: () => void }) {
  const { session } = useSession();
  const users = useApi<User[]>('/admin/users');
  const permissions = useApi<Permission[]>('/admin/permissions');
  const [editor, setEditor] = useState<{ user: User | null; mode: 'create' | 'edit' | 'permissions' } | null>(null);
  const [action, setAction] = useState<{ user: User; kind: AccountAction } | null>(null);
  const [secret, setSecret] = useState<Secret | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const protectedAccount = (u: User) => Boolean(u.is_judge) || u.permissions.some(key => key === 'admin.users' || !permissions.data?.find(p => p.key === key)?.admin_grantable);
  const changed = () => { users.reload(); onChanged(); };
  const confirm = async (reason: string) => {
    if (!action) return;
    setBusy(true); setError(null);
    try { const result = await api<Secret>('POST', `/admin/users/${action.user.id}/${action.kind}`, { reason }); if (result.temporary_password) setSecret(result); setAction(null); changed(); }
    catch (e) { setError(e); } finally { setBusy(false); }
  };
  return <Card title="Users" actions={<Button disabled={!permissions.data || permissions.loading || Boolean(permissions.error)} onClick={() => setEditor({ user: null, mode: 'create' })}>Create user</Button>}>
    <ErrorBanner error={users.error} onRetry={users.reload} /><ErrorBanner error={permissions.error} onRetry={permissions.reload} />
    {session.mode === 'demo' && <p>Demo people sign in with the persona switcher. New people cannot sign in, and passwords are not issued in demo mode.</p>}
    <p>You cannot edit your own account. Accounts with judicial or court-authority powers require the court CLI for permission changes, deactivation or credential resets. Deactivation ends sessions and assignments; reactivation does not restore assignments.</p>
    {users.loading ? <p role="status">Loading users…</p> : !users.error && users.data && <DataTable rows={users.data} rowKey={u => String(u.id)} empty="No users." columns={[
      { key: 'username', header: 'Username' }, { key: 'display_name', header: 'Name', render: u => <>{u.display_name}<div className="muted">{u.title}</div>{u.email && <div>{u.email}</div>}</> },
      { key: 'active', header: 'Account', render: u => <>{u.active ? 'Active' : 'Inactive'}<div>{u.mfa_enrolled ? 'Sign-in code enrolled' : 'No sign-in code'}</div></> },
      { key: 'active_sessions', header: 'Sessions', render: u => <>{u.active_sessions}{u.last_seen_at && <div className="muted">Last seen {fmtLocal(u.last_seen_at)}</div>}</> },
      { key: 'permissions', header: 'Actions', render: u => {
        const self = String(u.id) === String(session.user.id);
        const cli = protectedAccount(u);
        return <div className="admin-row-actions">
          <Button variant="secondary" disabled={self} onClick={() => setEditor({ user: u, mode: 'edit' })}>Edit</Button>
          <Button variant="secondary" disabled={self || !permissions.data || Boolean(permissions.error)} onClick={() => setEditor({ user: u, mode: 'permissions' })}>{cli ? 'View permissions' : 'Edit permissions'}</Button>
          {(['deactivate', 'reactivate', 'revoke-sessions', 'reset-password', 'reset-mfa'] as const).filter(kind => kind !== (u.active ? 'reactivate' : 'deactivate')).map(kind => <Button key={kind} variant="secondary" disabled={self || permissions.loading || Boolean(permissions.error) || (cli && ['deactivate', 'reset-password', 'reset-mfa'].includes(kind)) || (session.mode === 'demo' && kind === 'reset-password')} onClick={() => { setError(null); setAction({ user: u, kind }); }}>{actionLabels[kind]}</Button>)}
          {self && <span className="muted">Your account</span>}{cli && <span className="muted">Court CLI required for protected changes.</span>}
        </div>;
      } },
    ]} />}
    {editor && permissions.data && <UserEditor user={editor.user} mode={editor.mode} permissions={permissions.data} protectedAccount={Boolean(editor.user && protectedAccount(editor.user))} onClose={() => setEditor(null)} onSaved={changed} onSecret={setSecret} />}
    {action && <div className="admin-reason">
      <ConfirmReasonDialog open title={`${actionLabels[action.kind]}: ${action.user.display_name}`} label={action.kind === 'deactivate' ? 'Reason (all sessions and assignments will end)' : action.kind === 'reactivate' ? 'Reason (assignments must be restored separately)' : 'Reason for this account action'} confirmLabel={actionLabels[action.kind]} danger={action.kind === 'deactivate' || action.kind === 'revoke-sessions'} busy={busy} onConfirm={reason => void confirm(reason)} onClose={busy ? () => {} : () => { setAction(null); setError(null); }} />
      <ErrorBanner error={error} />
    </div>}
    {secret && <SecretDialog secret={secret} onClose={() => setSecret(null)} />}
  </Card>;
}

type RecordValue = string | number | boolean | null;
type AdminRecord = { id: number; [key: string]: RecordValue };
type FieldSpec = { key: string; label: string; type?: 'number' | 'textarea' | 'unit'; required?: boolean; immutable?: boolean; createOnly?: boolean; help?: string };
type ResourceSpec = { title: string; path: string; columns: { key: string; header: string }[]; fields: FieldSpec[] };
const referenceKinds = ['case_category', 'intake_channel', 'origin_island', 'document_type', 'closure_basis', 'hearing_type', 'participant_role', 'dispatch_method', 'relation_kind'];
const placeholders = ['court', 'recipient', 'case_number', 'case_title', 'hearing_local', 'hearing_type', 'room', 'previous_local', 'reason', 'intake_reference', 'received_date', 'missing_items', 'items'];
const unitSpec: ResourceSpec = { title: 'Court units', path: '/admin/court-units', columns: [{ key: 'code', header: 'Code' }, { key: 'name', header: 'Name' }], fields: [{ key: 'code', label: 'Code', required: true, immutable: true, help: '2–20 uppercase letters, digits or hyphens. Code is fixed after creation.' }, { key: 'name', label: 'Name', required: true }] };
const registrySpec: ResourceSpec = { title: 'Registers (number series)', path: '/admin/registries', columns: [{ key: 'series', header: 'Series' }, { key: 'name', header: 'Name' }, { key: 'court_unit_name', header: 'Court unit' }, { key: 'cases', header: 'Cases numbered' }], fields: [{ key: 'court_unit_id', label: 'Court unit', type: 'unit', required: true, createOnly: true }, { key: 'series', label: 'Number series', required: true, help: '2–20 uppercase letters, digits or hyphens. Once cases use the series, create a new register to change it.' }, { key: 'name', label: 'Name', required: true }] };
const roomSpec: ResourceSpec = { title: 'Rooms', path: '/admin/rooms', columns: [{ key: 'name', header: 'Name' }, { key: 'location', header: 'Location' }, { key: 'court_unit_name', header: 'Court unit' }], fields: [{ key: 'name', label: 'Name', required: true }, { key: 'location', label: 'Location' }, { key: 'court_unit_id', label: 'Court unit', type: 'unit', createOnly: true }] };
const refSpec: ResourceSpec = { title: 'Reference lists', path: '/admin/ref-items', columns: [{ key: 'code', header: 'Code' }, { key: 'label', header: 'Label' }, { key: 'sort', header: 'Sort order' }], fields: [{ key: 'code', label: 'Code', required: true, immutable: true, help: '2–40 lowercase letters, digits or underscores. Code cannot be changed.' }, { key: 'label', label: 'Label', required: true }, { key: 'sort', label: 'Sort order', type: 'number' }] };
const templateSpec: ResourceSpec = { title: 'Message templates', path: '/admin/templates', columns: [{ key: 'code', header: 'Code' }, { key: 'name', header: 'Name' }, { key: 'subject', header: 'Subject' }], fields: [{ key: 'code', label: 'Code', required: true, immutable: true, help: '2–40 lowercase letters, digits or underscores.' }, { key: 'name', label: 'Name', required: true }, { key: 'subject', label: 'Subject', required: true }, { key: 'body', label: 'Body', type: 'textarea', required: true }] };
function RecordEditor({ spec, row, kind, units, onClose, onSaved }: { spec: ResourceSpec; row: AdminRecord | null; kind?: string; units: AdminRecord[]; onClose: () => void; onSaved: () => void }) {
  const [values, setValues] = useState<Record<string, string>>(() => Object.fromEntries(spec.fields.map(f => [f.key, String(row?.[f.key] ?? (f.type === 'number' ? 0 : ''))])));
  const [active, setActive] = useState(row ? Boolean(row.active) : true);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const submit = async () => {
    const body: Record<string, RecordValue> = {};
    for (const f of spec.fields) {
      if (row && (f.immutable || f.createOnly)) continue;
      body[f.key] = f.type === 'number' ? Number(values[f.key]) : f.type === 'unit' ? values[f.key] ? Number(values[f.key]) : null : values[f.key] ?? '';
    }
    if (row) body.active = active;
    if (!row && kind) body.kind = kind;
    setBusy(true); setError(null);
    try { await api(row ? 'PATCH' : 'POST', `${spec.path}${row ? `/${row.id}` : ''}`, body); onSaved(); onClose(); }
    catch (e) { setError(e); } finally { setBusy(false); }
  };
  const fields = spec.fields.filter(f => !row || !f.createOnly);
  return <Modal title={`${row ? 'Edit' : 'Add'}: ${spec.title}`} open onClose={busy ? () => {} : onClose}>
    <ErrorBanner error={error} onRetry={() => void submit()} attempted={values} />
    {spec.path === '/admin/templates' && <p>Allowed placeholders: {placeholders.map(p => `{${p}}`).join(', ')}.</p>}
    <form onSubmit={e => { e.preventDefault(); void submit(); }}><fieldset className="admin-fieldset" disabled={busy}>
      {fields.map(f => {
        const set = (value: string) => setValues(v => ({ ...v, [f.key]: value }));
        const readOnly = Boolean(row && (f.immutable || (f.key === 'series' && Number(row.cases) > 0)));
        return f.type === 'unit' ? <SelectField key={f.key} label={f.label} value={values[f.key]} onChange={set} options={units.filter(u => u.active).map(u => ({ value: String(u.id), label: String(u.name) }))} placeholder="Choose a court unit" required={f.required} /> : f.type === 'textarea' ? <TextArea key={f.key} label={f.label} value={values[f.key]} onChange={set} required={f.required} rows={8} /> : <TextField key={f.key} label={f.label} value={values[f.key]} onChange={set} type={f.type === 'number' ? 'number' : 'text'} step={f.type === 'number' ? 1 : undefined} required={f.required} readOnly={readOnly} help={f.help} />;
      })}
      {row && <CheckboxField label="Active" checked={active} onChange={setActive} help="Inactive entries stay in historical records and are not offered for new work." />}
      <div className="actions"><Button type="submit" busy={busy}>Save changes</Button><Button type="button" variant="secondary" onClick={onClose}>Cancel</Button></div>
    </fieldset></form>
  </Modal>;
}
function Resource({ spec, kind, units = [], onChanged }: { spec: ResourceSpec; kind?: string; units?: AdminRecord[]; onChanged: () => void }) {
  const records = useApi<AdminRecord[]>(`${spec.path}${kind ? `?kind=${encodeURIComponent(kind)}` : ''}`);
  const [editor, setEditor] = useState<{ row: AdminRecord | null } | null>(null);
  const saved = () => { records.reload(); onChanged(); };
  return <Card title={spec.title} actions={<Button onClick={() => setEditor({ row: null })}>Add</Button>}>
    <ErrorBanner error={records.error} onRetry={records.reload} />
    {records.loading ? <p role="status">Loading {spec.title.toLowerCase()}…</p> : !records.error && records.data && <DataTable rows={records.data} rowKey={r => String(r.id)} empty="No entries yet." columns={[
      ...spec.columns,
      { key: 'active', header: 'Status', render: r => r.active ? 'Active' : 'Inactive' },
      { key: 'id', header: 'Actions', render: r => <Button variant="secondary" onClick={() => setEditor({ row: r })}>Edit</Button> },
    ]} />}
    {editor && <RecordEditor spec={spec} row={editor.row} kind={kind} units={units} onClose={() => setEditor(null)} onSaved={saved} />}
  </Card>;
}
type GeneralSettings = { court_name: string; hearing_buffer_minutes: number; intake_reference_prefix: string; timezone: string; mode: string };
function GeneralEditor({ initial, onChanged }: { initial: GeneralSettings; onChanged: () => void }) {
  const { refresh } = useSession();
  const [name, setName] = useState(initial.court_name);
  const [buffer, setBuffer] = useState(String(initial.hearing_buffer_minutes));
  const [prefix, setPrefix] = useState(initial.intake_reference_prefix);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const [saved, setSaved] = useState(false);
  const submit = async () => { setBusy(true); setError(null); setSaved(false); try { await api('PUT', '/admin/settings', { court_name: name, hearing_buffer_minutes: Number(buffer), intake_reference_prefix: prefix }); setSaved(true); onChanged(); await refresh(); } catch (e) { setError(e); } finally { setBusy(false); } };
  return <form onSubmit={e => { e.preventDefault(); void submit(); }}><ErrorBanner error={error} onRetry={() => void submit()} />
    <fieldset className="admin-fieldset" disabled={busy}><TextField label="Court name" value={name} onChange={setName} required /><TextField label="Hearing buffer (minutes)" type="number" value={buffer} onChange={setBuffer} min={0} max={240} step={1} required /><TextField label="Intake reference prefix" value={prefix} onChange={setPrefix} pattern="[A-Z]{1,6}" required help="1–6 uppercase letters." /><TextField label="Court timezone" value={initial.timezone} readOnly />
      <Button type="submit" busy={busy}>Save settings</Button>{saved && <p role="status">Settings saved.</p>}
    </fieldset>
  </form>;
}
function CourtSettings({ onChanged }: { onChanged: () => void }) {
  const units = useApi<AdminRecord[]>('/admin/court-units');
  const general = useApi<GeneralSettings>('/admin/settings');
  const [kind, setKind] = useState(referenceKinds[0] ?? 'case_category');
  return <>
    <Card title="General settings"><ErrorBanner error={general.error} onRetry={general.reload} />{general.loading ? <p role="status">Loading settings…</p> : !general.error && general.data && <GeneralEditor initial={general.data} onChanged={onChanged} />}</Card>
    <Resource spec={unitSpec} onChanged={() => { units.reload(); onChanged(); }} />
    <ErrorBanner error={units.error} onRetry={units.reload} />
    <Resource spec={registrySpec} units={units.data ?? []} onChanged={onChanged} />
    <Resource spec={roomSpec} units={units.data ?? []} onChanged={onChanged} />
    <SelectField label="Reference list" value={kind} onChange={setKind} options={referenceKinds.map(k => ({ value: k, label: k.replace(/_/g, ' ') }))} />
    <Resource key={kind} spec={refSpec} kind={kind} onChanged={onChanged} />
    <Resource spec={templateSpec} onChanged={onChanged} />
  </>;
}
export default function Settings() {
  const { session, hasPerm } = useSession();
  const ref = useRefData();
  return <><PageHeader title="Settings" />{hasPerm('admin.users') && <Users key={session.user.id} onChanged={ref.reload} />}{hasPerm('admin.settings') && <CourtSettings key={session.user.id} onChanged={ref.reload} />}{!hasPerm('admin.users') && !hasPerm('admin.settings') && <p>You do not have permission to manage users or settings.</p>}</>;
}
