/**
 * Dispatch tab (C09/C12): every prepared notice and copy package of the case,
 * each as a full DispatchCard. "Prepare notice" / "Prepare copies" open the
 * DispatchForm — one package per participant, review before sending.
 */

import { useEffect, useState } from 'react';
import { useSearchParams } from 'react-router-dom';
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
  // Next-action deep links: ?action=copies&decision={id}&party={id} opens the
  // copies form pre-filled; ?dispatch={id} scrolls to and marks that card.
  const [params] = useSearchParams();
  const actionParam = params.get('action');
  const dispatchParam = params.get('dispatch');
  const [prepare, setPrepare] = useState<'notice' | 'copies' | null>(() =>
    actionParam === 'copies' || actionParam === 'notice' ? actionParam : null,
  );
  const [preselect] = useState(() => ({
    decision: params.get('decision') ? Number(params.get('decision')) : undefined,
    party: params.get('party') ? Number(params.get('party')) : undefined,
  }));
  const [highlight, setHighlight] = useState<number | null>(
    dispatchParam ? Number(dispatchParam) : null,
  );

  useEffect(() => {
    if (!dispatchParam || !data) return;
    const el = document.getElementById(`dispatch-${dispatchParam}`);
    el?.scrollIntoView({ block: 'start' });
  }, [dispatchParam, data]);

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
        <div
          key={d.id}
          id={`dispatch-${d.id}`}
          className={highlight === d.id ? 'dispatch-anchor dispatch-anchor--on' : 'dispatch-anchor'}
        >
          <DispatchCard dispatch={d} onChanged={changed} />
        </div>
      ))}

      {prepare && (
        <DispatchForm
          caseId={caseId}
          participants={caseData.participants}
          kind={prepare}
          preselectDecisionId={preselect.decision}
          preselectPartyId={preselect.party}
          onClose={() => { setPrepare(null); setHighlight(null); }}
          onSaved={() => {
            setPrepare(null);
            changed();
          }}
        />
      )}
    </>
  );
}
