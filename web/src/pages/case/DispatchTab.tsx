/**
 * Dispatch tab — minimal read-only list for now. Another agent replaces this
 * with prepare/review/queue/confirm UI. Keep the props contract
 * `{caseId, caseData, reload}` when rewriting.
 */

import { makeListTab } from './sublist';

export default makeListTab(
  'Dispatches',
  (caseId) => `/cases/${caseId}/dispatches`,
  'No dispatches prepared for this case.',
);
