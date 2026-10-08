/**
 * Hearings tab — minimal read-only list for now. Another agent replaces this
 * with scheduling/confirmation/adjournment. Keep the props contract
 * `{caseId, caseData, reload}` when rewriting.
 */

import { makeListTab } from './sublist';

export default makeListTab(
  'Hearings',
  (caseId) => `/cases/${caseId}/hearings`,
  'No hearings on this case yet.',
);
