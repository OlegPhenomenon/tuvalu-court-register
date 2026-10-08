/**
 * Mailbox (C09): the LOCAL e-mail viewer. Method `email` dispatches are
 * delivered here by the outbox worker — nothing leaves the server, no real
 * recipient is ever contacted. Reading pane shows the message as preformatted
 * text (never HTML); attachment downloads go through
 * /api/document-versions/{id}/download, which re-checks access on every hit.
 */

import { useMemo, useState } from 'react';
import { Link, useSearchParams } from 'react-router-dom';
import { downloadUrl } from '../api';
import { Card } from '../components/Card';
import { ErrorBanner } from '../components/ErrorBanner';
import { PageHeader } from '../components/PageHeader';
import { useApi } from '../components/useApi';
import { fmtLocal } from '../time';
import './dispatch.css';

interface MailAttachment {
  filename?: string;
  sha256?: string;
  size_bytes?: number;
  /** Missing/unknown when the version was redacted or the row predates version ids. */
  document_version_id?: number | null;
  /** The server redacts versions the viewer may not see. */
  restricted?: boolean;
}

/** GET /api/mailbox item (mailbox.rs). */
interface MailMessage {
  id: number;
  dispatch_id: number;
  case_number: string | null;
  attempt_no: number;
  to_address: string;
  subject: string;
  body: string;
  attachments: MailAttachment[];
  delivered_at: string;
}

function fmtSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
}

export default function Mailbox() {
  const [params] = useSearchParams();
  const dispatchFilter = params.get('dispatch');
  const path = dispatchFilter ? `/mailbox?dispatch_id=${dispatchFilter}` : '/mailbox';
  const { data, error, loading, reload } = useApi<{ items: MailMessage[] }>(path);
  const [selectedId, setSelectedId] = useState<number | null>(null);

  const items = data?.items ?? [];
  const selected = useMemo(
    () => items.find((m) => m.id === selectedId) ?? items[0] ?? null,
    [items, selectedId],
  );

  return (
    <>
      <PageHeader title="Mailbox" />
      <div className="banner mailbox-banner" role="note">
        <p>
          <strong>Nothing leaves this server.</strong> Messages that would have been e-mailed appear
          here.
        </p>
      </div>
      {dispatchFilter && (
        <p className="muted">
          Showing messages of one dispatch. <Link to="/mailbox">Show the whole mailbox</Link>
        </p>
      )}
      {loading && <p className="muted">Loading…</p>}
      <ErrorBanner error={error} onRetry={reload} />
      {data && items.length === 0 && (
        <Card>
          <p className="muted">The local mailbox is empty.</p>
        </Card>
      )}
      {items.length > 0 && (
        <div className="mailbox-layout">
          <div className="mailbox-list" role="list" aria-label="Messages">
            {items.map((m) => (
              <button
                key={m.id}
                type="button"
                role="listitem"
                className={selected?.id === m.id ? 'mailbox-item active' : 'mailbox-item'}
                onClick={() => setSelectedId(m.id)}
              >
                <span className="mailbox-item-subject">{m.subject}</span>
                <span className="mailbox-item-meta">
                  to {m.to_address || '—'}
                  {m.case_number ? ` · ${m.case_number}` : ''}
                </span>
                <span className="mailbox-item-meta">{fmtLocal(m.delivered_at)}</span>
              </button>
            ))}
          </div>
          {selected && (
            <Card>
              <div className="table-wrap">
                <table>
                  <tbody>
                    <tr>
                      <th scope="row">To</th>
                      <td>{selected.to_address || '—'}</td>
                    </tr>
                    <tr>
                      <th scope="row">Subject</th>
                      <td>{selected.subject}</td>
                    </tr>
                    <tr>
                      <th scope="row">Delivered</th>
                      <td>{fmtLocal(selected.delivered_at)}</td>
                    </tr>
                    {selected.case_number && (
                      <tr>
                        <th scope="row">Case</th>
                        <td>{selected.case_number}</td>
                      </tr>
                    )}
                  </tbody>
                </table>
              </div>
              {/* Plain text only — the server stores text bodies; never render as HTML. */}
              <pre className="mailbox-body">{selected.body}</pre>
              {selected.attachments.length > 0 && (
                <div className="dispatch-section">
                  <h3>Attachments</h3>
                  <ul className="dispatch-items">
                    {selected.attachments.map((a, i) => (
                      <li key={a.document_version_id ?? i}>
                        {a.restricted || !a.document_version_id ? (
                          <span className="muted">Restricted document (not shown)</span>
                        ) : (
                          <>
                            <a
                              href={downloadUrl(`/document-versions/${a.document_version_id}/download`)}
                              download
                            >
                              {a.filename}
                            </a>{' '}
                            {typeof a.size_bytes === 'number' && (
                              <span className="muted">({fmtSize(a.size_bytes)})</span>
                            )}
                          </>
                        )}
                      </li>
                    ))}
                  </ul>
                </div>
              )}
            </Card>
          )}
        </div>
      )}
    </>
  );
}
