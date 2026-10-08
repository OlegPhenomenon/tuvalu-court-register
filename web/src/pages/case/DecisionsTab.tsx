/**
 * Decisions tab — minimal read-only list for now. Another agent replaces this
 * with draft/finalise/amend UI. Keep the props contract
 * `{caseId, caseData, reload}` when rewriting.
 */

import { makeListTab } from './sublist';

export default makeListTab(
  'Decisions',
  (caseId) => `/cases/${caseId}/decisions`,
  'No decisions on this case yet.',
);
