/**
 * Dispatch tab (C09/C12): every prepared notice and copy package of the case,
 * each as a full DispatchCard. "Prepare notice" / "Prepare copies" open the
 * DispatchForm — one package per participant, review before sending.
 */

import { useState } from 'react';
import { Button } from '../../components/Button';
import { DispatchCard } from '../../components/DispatchCard';
import type { DispatchRecord } from '../../components/DispatchCard';
import { DispatchForm } from '../../components/DispatchForm';
import { ErrorBanner } from '../../components/ErrorBanner';
import { useApi } from '../../components/useApi';
import type { CaseTabProps } from './types';
import '../dispatch.css';

export default function DispatchTab({ caseId, caseData, reload }: CaseTabProps) {
  const { data, error, loading, reload: reloadList } = useApi<{ items: DispatchRecord[] }>(
    `/cases/${caseId}/dispatches`,
  );
  const [prepare, setPrepare] = useState<'notice' | 'copies' | null>(null);

  const changed = () => {
    reloadList();
    reload(); // refresh next_actions / allowed
  };

  return (
    <>
      {caseData.allowed.dispatch && (
        <div className="page-actions" style={{ marginBottom: '1rem' }}>
          <Button onClick={() => setPrepare('notice')}>Prepare notice</Button>
          <Button variant="secondary" onClick={() => setPrepare('copies')}>Prepare copies</Button>
        </div>
      )}

      {loading && <p className="muted">Loading…</p>}
      <ErrorBanner error={error} onRetry={reloadList} />
      {data && data.items.length === 0 && (
        <p className="muted">No notices or copy packages prepared for this case yet.</p>
      )}
      {data?.items.map((d) => (
        <DispatchCard key={d.id} dispatch={d} onChanged={changed} />
      ))}

      {prepare && (
        <DispatchForm
          caseId={caseId}
          participants={caseData.participants}
          kind={prepare}
          onClose={() => setPrepare(null)}
          onSaved={() => {
            setPrepare(null);
            changed();
          }}
        />
      )}
    </>
  );
}
