import { useRef } from 'react';

/** Automatically activated tabs, linked to panels with `${idPrefix}-panel-${key}` IDs. */
export function Tabs({ tabs, active, onChange, idPrefix }: {
  tabs: { key: string; label: string }[];
  active: string;
  onChange: (key: string) => void;
  idPrefix: string;
}) {
  const ref = useRef<HTMLDivElement>(null);

  return (
    <div className="tabs" role="tablist" aria-label="Case sections" ref={ref}>
      {tabs.map((t, index) => (
        <button
          key={t.key}
          type="button"
          role="tab"
          id={`${idPrefix}-tab-${t.key}`}
          aria-controls={`${idPrefix}-panel-${t.key}`}
          aria-selected={t.key === active}
          tabIndex={t.key === active ? 0 : -1}
          className={t.key === active ? 'tab active' : 'tab'}
          onClick={() => onChange(t.key)}
          onKeyDown={(event) => {
            let next: number;
            switch (event.key) {
              case 'ArrowLeft': next = (index - 1 + tabs.length) % tabs.length; break;
              case 'ArrowRight': next = (index + 1) % tabs.length; break;
              case 'Home': next = 0; break;
              case 'End': next = tabs.length - 1; break;
              default: return;
            }
            event.preventDefault();
            ref.current?.querySelectorAll<HTMLButtonElement>('[role="tab"]')[next]?.focus();
            onChange(tabs[next]!.key);
          }}
        >
          {t.label}
        </button>
      ))}
    </div>
  );
}
