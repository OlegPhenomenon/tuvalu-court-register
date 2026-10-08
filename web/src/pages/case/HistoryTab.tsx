/**
 * History tab — minimal read-only list for now. Another agent replaces this
 * with the audit/history view. Keep the props contract
 * `{caseId, caseData, reload}` when rewriting.
 */

import { makeListTab } from './sublist';

export default makeListTab(
  'History',
  (caseId) => `/cases/${caseId}/history`,
  'No history entries.',
);
