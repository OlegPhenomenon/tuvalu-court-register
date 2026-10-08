import { useState } from 'react';
import type { FormEvent } from 'react';
import { Link } from 'react-router-dom';
import { Button } from '../components/Button';
import { Card } from '../components/Card';
import { DataTable } from '../components/DataTable';
import { ErrorBanner } from '../components/ErrorBanner';
import { PageHeader } from '../components/PageHeader';
import { SelectField, TextField } from '../components/fields';
import { options, refList, useRef as useRefData } from '../components/refdata';
import { useApi } from '../components/useApi';
import { DocumentLinks, VisibilityBadge, visibilityOptions, useDocumentDetails } from '../components/DocumentUpload';
import { fmtDate } from '../time';
import { useSession } from '../session';
import './documents.css';

export default function Documents() {
  const { session } = useSession();
  return <DocumentsList key={session.user.id} />;
}

function DocumentsList() {
  const { data: ref, error: refError, reload: reloadRef } = useRefData();
  const cases = useApi<{ items: { id: number; number: string; title: string }[] }>('/cases');
  const [q, setQ] = useState('');
  const [caseId, setCaseId] = useState('');
  const [type, setType] = useState('');
  const [visibility, setVisibility] = useState('');
  const [query, setQuery] = useState('');
  const docs = useDocumentDetails(`/documents${query ? `?${query}` : ''}`);
  const search = (e: FormEvent) => {
    e.preventDefault();
    const params = new URLSearchParams();
    if (q.trim()) params.set('q', q.trim());
    if (caseId) params.set('case_id', caseId);
    if (type) params.set('doc_type', type);
    if (visibility) params.set('visibility', visibility);
    if (params.toString() === query) docs.reload(); else setQuery(params.toString());
  };
  return <div className="doc-record">
    <PageHeader title="Documents" />
    <p>Search the documents you can access by title or filename. Access to a case does not include restricted documents.</p>
    <Card>
      <ErrorBanner error={refError} onRetry={reloadRef} /><ErrorBanner error={cases.error} onRetry={cases.reload} />
      <form onSubmit={search} className="doc-filters">
        <TextField label="Title or filename" value={q} onChange={setQ} type="search" />
        <SelectField label="Case" value={caseId} onChange={setCaseId} placeholder="All accessible cases" options={(cases.data?.items ?? []).map((c) => ({ value: String(c.id), label: `${c.number} — ${c.title}` }))} />
        <SelectField label="Document type" value={type} onChange={setType} placeholder="All types" options={options(refList(ref, 'document_type'))} />
        <SelectField label="Visibility" value={visibility} onChange={setVisibility} placeholder="All visibility levels" options={visibilityOptions} />
        <Button type="submit">Search</Button>
      </form>
      <ErrorBanner error={docs.error} onRetry={docs.reload} />
      {docs.loading ? <p role="status">Loading documents…</p> : <DataTable rows={docs.data ?? []} rowKey={(d) => String(d.id)} empty={docs.error ? 'Documents could not be loaded.' : 'No accessible documents match these filters.'} columns={[
        { key: 'title', header: 'Document', render: (d) => <>{d.title}<div className="muted">{d.doc_type_label}</div></> },
        { key: 'case_id', header: 'Case / Filing', render: (d) => d.case_id ? <Link to={`/cases/${d.case_id}?tab=documents`}>{d.case_number}</Link> : d.intake_id ? <Link to={`/intakes/${d.intake_id}`}>Filing #{d.intake_id}</Link> : '—' },
        { key: 'visibility', header: 'Visibility', render: (d) => <VisibilityBadge value={d.visibility} /> },
        { key: 'document_date', header: 'Document / Received date', render: (d) => <>{d.document_date ? fmtDate(d.document_date) : '—'}<div>{d.received_date ? fmtDate(d.received_date) : '—'}</div></> },
        { key: 'version_count', header: 'Latest file', render: (d) => { const v = d.versions.at(-1); return v ? <>{v.filename} · v{v.version_no}<div className="muted">{d.version_count} version(s)</div><DocumentLinks version={v} /></> : 'No file'; } },
        { key: 'is_paper_original', header: 'Paper original', render: (d) => d.is_paper_original ? d.original_location ?? 'Location not recorded' : 'Not received' },
      ]} />}
      <p className="muted">Up to 500 results. Narrow the filters to find older material.</p>
    </Card>
  </div>;
}
