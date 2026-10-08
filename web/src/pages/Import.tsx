import { useState } from 'react';
import { Link } from 'react-router-dom';
import { api, newKey, upload } from '../api';
import { useSession } from '../session';
import { fmtLocal } from '../time';
import { PageHeader } from '../components/PageHeader';
import { Card } from '../components/Card';
import { Button } from '../components/Button';
import { DataTable } from '../components/DataTable';
import { ErrorBanner } from '../components/ErrorBanner';
import { SelectField } from '../components/fields';
import { Modal } from '../components/Modal';
import { useRef as useRefData } from '../components/refdata';
import { useApi } from '../components/useApi';
import './admin.css';

const CASE_HEADER = 'number,category,title,registered_date,status,responsible_username,closed_date,closure_basis,parties';
const FILE_HEADER = 'case_number,filename,title,doc_type,visibility,document_date';
type PreviewRow = { row: number; number?: string; target_number?: string | null; legacy_number?: string | null; case_number?: string; filename?: string | null; title?: string | null; restricted?: boolean; action: 'create' | 'skip_existing' | 'error'; problems: string[]; missing: string[] };
type Preview = { batch_id?: number; summary: { create: number; skip_existing: number; error: number }; rows: PreviewRow[] };
type Result = { batch_id: number; status: string; summary: { created: number; skip_existing: number; error: number }; created: { case_id: number; number?: string; document_id?: number; version_id?: number; restricted?: boolean }[] };
type Batch = { id: number; kind: string; filename: string; status: string; created_at: string; committed_at: string | null };
type Detail = { batch_id: number; kind: string; filename: string; status: string; own: boolean; can_commit: boolean; preview: Preview; result: Result | null };
function template(header: string, filename: string) {
  const url = URL.createObjectURL(new Blob([`${header}\r\n`], { type: 'text/csv;charset=utf-8' }));
  const link = document.createElement('a'); link.href = url; link.download = filename;
  document.body.append(link); link.click(); link.remove(); setTimeout(() => URL.revokeObjectURL(url), 1000);
}
function PreviewTable({ preview }: { preview: Preview }) {
  return <>
    <p>{preview.summary.create} to create · {preview.summary.skip_existing} existing rows to skip · {preview.summary.error} rows with errors.</p>
    <p>Only rows marked Create will be imported. Existing records and error rows are skipped. Missing historical values remain missing and are flagged; no facts are invented.</p>
    <DataTable rows={preview.rows} rowKey={r => String(r.row)} empty="The source has no rows." columns={[
      { key: 'row', header: 'CSV row' },
      { key: 'number', header: 'Case number', render: r => <>{r.number ?? r.case_number}{r.legacy_number && <div className="muted">Kept as legacy number; a new number will be allocated.</div>}</> },
      { key: 'filename', header: 'File / title', render: r => r.restricted ? <span className="muted">Restricted document</span> : <>{r.filename ?? '—'}{r.title && <div>{r.title}</div>}</> },
      { key: 'action', header: 'Row action', render: r => <span className={`import-action import-action--${r.action}`}>{r.action === 'skip_existing' ? 'Skip existing' : r.action === 'error' ? 'Error — skip' : 'Create'}</span> },
      { key: 'problems', header: 'Problems', render: r => r.problems.length ? <ul className="import-problems">{r.problems.map((p, i) => <li key={i}>{p}</li>)}</ul> : 'None' },
      { key: 'missing', header: 'Missing values', render: r => r.missing.length ? <ul className="import-missing">{r.missing.map(m => <li key={m}>{m.replace(/_/g, ' ')}</li>)}</ul> : 'None' },
    ]} />
  </>;
}
function ImportResult({ result }: { result: Result }) {
  return <div role="status"><p>Batch #{result.batch_id} committed: {result.summary.created} created, {result.summary.skip_existing} existing rows skipped, {result.summary.error} error rows skipped.</p>
    <ul>{result.created.map((r, i) => <li key={i}><Link to={`/cases/${r.case_id}${r.document_id || r.restricted ? '?tab=documents' : ''}`}>{r.number ?? `Case #${r.case_id}`}{r.document_id ? ` — document #${r.document_id}` : r.restricted ? ' — restricted document' : ''}</Link></li>)}</ul>
  </div>;
}
function CommitPreview({ batchId, preview, initialResult, canCommit = true, onCommitted }: { batchId: number; preview: Preview; initialResult?: Result | null; canCommit?: boolean; onCommitted: () => void }) {
  const [key] = useState(newKey);
  const [result, setResult] = useState(initialResult ?? null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const commit = async () => { setBusy(true); setError(null); try { setResult(await api<Result>('POST', `/import/${batchId}/commit`, undefined, { idempotencyKey: key })); onCommitted(); } catch (e) { setError(e); } finally { setBusy(false); } };
  return <>
    <PreviewTable preview={preview} />
    <ErrorBanner error={error} onRetry={() => void commit()} />
    {result ? <ImportResult result={result} /> : canCommit ? <div className="actions"><Button busy={busy} disabled={!preview.summary.create} onClick={() => void commit()}>Commit import</Button></div>
      : <p className="muted">This package contains restricted material. Only the person who uploaded it can commit it.</p>}
  </>;
}
function Wizard({ zip, onCommitted }: { zip: boolean; onCommitted: () => void }) {
  const ref = useRefData();
  const [file, setFile] = useState<File | null>(null);
  const [registry, setRegistry] = useState('');
  const [preview, setPreview] = useState<Preview | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>(null);
  const previewFile = async () => {
    if (!file) return;
    setBusy(true); setError(null);
    try { const form = new FormData(); form.append('file', file); if (!zip && registry) form.append('registry_id', registry); setPreview(await upload<Preview>(`/import/${zip ? 'files' : 'cases'}/preview`, form)); onCommitted(); }
    catch (e) { setError(e); } finally { setBusy(false); }
  };
  return <Card title={zip ? 'Document package ZIP' : 'Legacy cases CSV'}>
    <ol className="import-steps"><li>Upload source</li><li>Review preview</li><li>Commit and review result</li></ol>
    {zip ? <>
      <p>Use a ZIP with <code>manifest.csv</code> at its root and the files named by each row. The manifest must be UTF-8 with this exact header:</p><pre className="admin-json">{FILE_HEADER}</pre>
      <p>Use an accessible current or legacy case number. Filename is the relative archive path. Types use document-type codes; visibility is administrative, party_material, restricted or judicial_note. Dates use YYYY-MM-DD; document_date may be blank. Files must be PDF, DOCX, JPEG or PNG. Limit: 20 MB ZIP, 15 MB per file, 40 MB expanded, 500 archive entries. Packages with restricted rows or judicial notes can be committed only by the person who uploaded them; others see those rows as restricted.</p>
      <Button variant="secondary" onClick={() => template(FILE_HEADER, 'manifest.csv')}>Download manifest CSV template</Button>
    </> : <>
      <p>Upload UTF-8 CSV (up to 5 MB) with this exact header:</p><pre className="admin-json">{CASE_HEADER}</pre>
      <p>Dates use YYYY-MM-DD. Category and closure basis use reference codes. Status: registered, active, on_hold, closed or reopened. The responsible username must identify active staff. Parties use <code>Name (role); Name (role)</code>, with participant-role codes.</p>
      <Button variant="secondary" onClick={() => template(CASE_HEADER, 'legacy-cases-template.csv')}>Download CSV template</Button>
      <ErrorBanner error={ref.error} onRetry={ref.reload} />
    </>}
    <form onSubmit={e => { e.preventDefault(); void previewFile(); }}>
      <label className="field">{zip ? 'ZIP package' : 'CSV source'}<input className="input" type="file" accept={zip ? '.zip,application/zip' : '.csv,text/csv'} required disabled={busy} onChange={e => { setFile(e.target.files?.[0] ?? null); setPreview(null); setError(null); }} /></label>
      {!zip && <SelectField label="Register for non-series legacy numbers" value={registry} onChange={r => { setRegistry(r); setPreview(null); }} placeholder="Select if legacy numbers need a new series" disabled={busy} help="Recognised SERIES-YYYY-NNNN numbers are retained. Other numbers remain as legacy numbers and receive a new number from this register." options={(ref.data?.registries ?? []).map(r => ({ value: String(r.id), label: `${r.series} — ${r.name}` }))} />}
      <ErrorBanner error={error} onRetry={() => void previewFile()} />
      <div className="actions"><Button type="submit" busy={busy} disabled={!file}>Preview upload</Button></div>
    </form>
    {preview?.batch_id && <CommitPreview key={preview.batch_id} batchId={preview.batch_id} preview={preview} onCommitted={onCommitted} />}
  </Card>;
}
function BatchDialog({ id, onClose, onCommitted }: { id: number; onClose: () => void; onCommitted: () => void }) {
  const detail = useApi<Detail>(`/import/${id}`);
  return <Modal title={`Import batch #${id}`} open onClose={onClose}>
    <ErrorBanner error={detail.error} onRetry={detail.reload} />
    {detail.loading ? <p role="status">Loading batch…</p> : !detail.error && detail.data && <><p>{detail.data.filename} · {detail.data.status}{!detail.data.own && ' · uploaded by another user'}</p><CommitPreview batchId={id} preview={detail.data.preview} initialResult={detail.data.result} canCommit={detail.data.can_commit} onCommitted={onCommitted} /></>}
  </Modal>;
}
function ImportScreen() {
  const batches = useApi<{ batches: Batch[] }>('/import');
  const [opened, setOpened] = useState<number | null>(null);
  return <>
    <Wizard zip={false} onCommitted={batches.reload} /><Wizard zip onCommitted={batches.reload} />
    <Card title="Import batches"><ErrorBanner error={batches.error} onRetry={batches.reload} />
      {batches.loading ? <p role="status">Loading batches…</p> : !batches.error && batches.data && <DataTable rows={batches.data.batches} rowKey={r => String(r.id)} empty="No import batches yet." columns={[
        { key: 'id', header: 'Batch', render: r => <Button variant="secondary" onClick={() => setOpened(r.id)}>#{r.id}</Button> },
        { key: 'kind', header: 'Source', render: r => r.kind === 'cases_csv' ? 'Legacy cases CSV' : 'Document package ZIP' }, { key: 'filename', header: 'Filename' }, { key: 'status', header: 'Status' }, { key: 'created_at', header: 'Previewed', render: r => fmtLocal(r.created_at) }, { key: 'committed_at', header: 'Committed', render: r => r.committed_at ? fmtLocal(r.committed_at) : '—' },
      ]} />}
    </Card>
    {opened !== null && <BatchDialog key={opened} id={opened} onClose={() => setOpened(null)} onCommitted={batches.reload} />}
  </>;
}
export default function Import() { const { session, hasPerm } = useSession(); return <><PageHeader title="Import" />{hasPerm('import.run') ? <ImportScreen key={session.user.id} /> : <p>You do not have permission to import materials.</p>}</>; }
