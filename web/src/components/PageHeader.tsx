import type { ReactNode } from 'react';

/** Page heading row: `<PageHeader title="Cases" actions={<Button/>} />`. */
export function PageHeader({ title, actions }: { title: ReactNode; actions?: ReactNode }) {
  return (
    <header className="page-header">
      <h1>{title}</h1>
      {actions && <div className="page-actions">{actions}</div>}
    </header>
  );
}
