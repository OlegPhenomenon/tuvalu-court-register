/**
 * Tasks tab — minimal read-only list for now. Another agent replaces this
 * with create/complete/cancel/carry-forward UI. Keep the props contract
 * `{caseId, caseData, reload}` when rewriting.
 */

import { makeListTab } from './sublist';

export default makeListTab(
  'Tasks',
  (caseId) => `/cases/${caseId}/tasks`,
  'No tasks on this case.',
);
