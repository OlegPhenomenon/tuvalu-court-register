import { useEffect, useState } from 'react';
import { api, ApiError } from '../../api';
import { useSession } from '../../session';
import { Button } from '../../components/Button';
import { Modal } from '../../components/Modal';
import { TextArea, CheckboxField } from '../../components/fields';
import { ErrorBanner } from '../../components/ErrorBanner';
import { fmtLocal } from '../../time';
import '../admin.css';

type Version = { id: number; version_no: number; filename: string; scan_status: string; uploaded_at: string };
type Document = { id: number; title: string; visibility: string; versions: Version[] };
export default function ExportDialog({ caseId, onClose }: { caseId: number; onClose: () => void }) {
  const { session } = useSession();
  const [purpose, setPurpose] = useState('');
  const [documents, setDocuments] = useState<Document[]>([]);
  const [selected, setSelected] = useState<number[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const [loadError, setLoadError] = useState<unknown>(null);
  const [error, setError] = useState<unknown>(null);
  const [tick, setTick] = useState(0);
  const [downloaded, setDownloaded] = useState(false);
  useEffect(() => {
    const ac = new AbortController();
    setLoading(true); setLoadError(null);
    const load = async () => {
      try {
        const list = await api<{ items: Omit<Document, 'versions'>[] }>('GET', `/cases/${caseId}/documents`, undefined, { signal: ac.signal });
        const responses = await Promise.allSettled(list.items.filter(d => d.visibility !== 'judicial_note').map(d => api<Document>('GET', `/documents/${d.id}`, undefined, { signal: ac.signal })));
        if (ac.signal.aborted) return;
        const docs: Document[] = [];
        let failure: unknown = null;
        for (const r of responses) { if (r.status === 'fulfilled') docs.push(r.value); else failure = r.reason; }
        if (failure) throw failure;
        setDocuments(docs.filter(d => d.visibility !== 'judicial_note'));
        setSelected(docs.filter(d => ['administrative', 'party_material'].includes(d.visibility)).flatMap(d => {
          const latest = d.versions.filter(v => v.scan_status === 'clean').sort((a, b) => b.version_no - a.version_no)[0];
          return latest ? [latest.id] : [];
        }));
      } catch (e) { if (!ac.signal.aborted) setLoadError(e); }
      finally { if (!ac.signal.aborted) setLoading(false); }
    };
    void load();
    return () => ac.abort();
  }, [caseId, tick, session]);
  const exportPackage = async () => {
    setBusy(true); setError(null); setDownloaded(false);
    try {
      const response = await fetch(`/api/cases/${caseId}/export`, { method: 'POST', credentials: 'same-origin', headers: { 'X-TCR': '1', 'Content-Type': 'application/json' }, body: JSON.stringify({ purpose: purpose.trim(), version_ids: selected }) });
      if (!response.ok) {
        const body = await response.json().catch(() => null) as { error?: { code: string; message: string; details?: unknown } } | null;
        throw new ApiError(response.status, body?.error?.code ?? `http_${response.status}`, body?.error?.message ?? 'Could not build the case package.', body?.error?.details);
      }
      const blob = await response.blob();
      const url = URL.createObjectURL(blob);
      const link = document.createElement('a');
      link.href = url;
      link.download = response.headers.get('Content-Disposition')?.match(/filename="([^"]+)"/)?.[1] ?? `case-${caseId}-export.zip`;
      document.body.append(link); link.click(); link.remove();
      setTimeout(() => URL.revokeObjectURL(url), 1000);
      setDownloaded(true);
    } catch (e) { setError(e); }
    finally { setBusy(false); }
  };
  return <Modal title="Export package" open onClose={busy ? () => {} : onClose}>
    <p>This package contains only permitted materials. It is not a backup.</p>
    <p>The package includes case details, participants, hearings, permitted decisions, relationships and chronology, plus the document versions you choose.</p>
    <ErrorBanner error={loadError} onRetry={() => setTick(t => t + 1)} />
    <ErrorBanner error={error} onRetry={() => void exportPackage()} />
    {downloaded && <p role="status">Package downloaded. The purpose and selected versions were recorded in the audit log.</p>}
    <form onSubmit={e => { e.preventDefault(); void exportPackage(); }}>
      <TextArea label="Purpose" value={purpose} onChange={setPurpose} required disabled={busy} rows={3} />
      {loading ? <p role="status">Loading permitted document versions…</p> : !loadError && <fieldset disabled={busy} className="admin-fieldset"><legend>Choose clean document versions</legend>
        <p className="muted">The latest clean version of each ordinary document is selected. Restricted versions require an explicit choice. Judicial notes are never offered.</p>
        {documents.map(d => <div key={d.id} className="export-document"><h3>{d.title}{d.visibility === 'restricted' ? ' — Restricted' : ''}</h3>
          {d.versions.filter(v => v.scan_status === 'clean').map(v => <CheckboxField key={v.id} label={`Version ${v.version_no}: ${v.filename}${d.visibility === 'restricted' ? ' (include restricted material)' : ''}`} help={`Uploaded ${fmtLocal(v.uploaded_at)}`} checked={selected.includes(v.id)} onChange={checked => setSelected(ids => checked ? [...ids, v.id] : ids.filter(id => id !== v.id))} />)}
          {!d.versions.some(v => v.scan_status === 'clean') && <p>No clean versions available.</p>}
        </div>)}
        {!documents.length && <p>No exportable documents. You may export the permitted case information alone.</p>}
      </fieldset>}
      <div className="actions"><Button type="submit" busy={busy} disabled={!purpose.trim() || loading || Boolean(loadError)}>Download ZIP</Button><Button type="button" variant="secondary" disabled={busy} onClick={onClose}>Close</Button></div>
    </form>
  </Modal>;
}
