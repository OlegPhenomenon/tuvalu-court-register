/**
 * Documents tab — minimal read-only list for now. Another agent replaces this
 * with the full upload/version/visibility UI. Keep the props contract
 * `{caseId, caseData, reload}` when rewriting.
 */

import { makeListTab } from './sublist';

export default makeListTab(
  'Documents',
  (caseId) => `/cases/${caseId}/documents`,
  'No documents on this case yet.',
);
