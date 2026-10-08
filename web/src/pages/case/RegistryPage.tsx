import type { ReactNode } from 'react';

/** Keep wide registry tables within the printed page, with repeatable headings. */
export function RegistryPage({ children }: { children: ReactNode }) {
  return <div className="tcr-registry-page" style={{ overflowWrap: 'anywhere' }}>
    <style>{`@media print {
      .tcr-registry-page .table-wrap { overflow: visible; }
      .tcr-registry-page table { table-layout: fixed; font-size: 9pt; }
      .tcr-registry-page th, .tcr-registry-page td {
        white-space: normal; overflow-wrap: anywhere; padding: 0.2rem;
      }
      .tcr-registry-page .badge { white-space: normal; }
      .tcr-registry-page thead { display: table-header-group; }
      .tcr-registry-page tr { break-inside: avoid; }
      .tcr-registry-page .tabs, .tcr-registry-page form,
      .tcr-registry-page .modal-backdrop { display: none; }
    }`}</style>
    {children}
  </div>;
}
