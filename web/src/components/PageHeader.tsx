import type { ReactNode } from 'react';

/**
 * Page heading row: `<PageHeader title="Cases" subtitle="…" actions={<Button/>} />`.
 * `eyebrow` is a small label above the title (e.g. "Case").
 */
export function PageHeader({ title, subtitle, eyebrow, actions }: {
  title: ReactNode;
  subtitle?: ReactNode;
  eyebrow?: ReactNode;
  actions?: ReactNode;
}) {
  return (
    <header className="page-header">
      <div className="page-heading">
        {eyebrow && <p className="page-eyebrow">{eyebrow}</p>}
        <h1>{title}</h1>
        {subtitle && <p className="page-sub">{subtitle}</p>}
      </div>
      {actions && <div className="page-actions">{actions}</div>}
    </header>
  );
}
