/** Tab strip (button-based, aria tablist): `<Tabs tabs={[{key,label}]} active={k} onChange={setK} />`. */
export function Tabs({ tabs, active, onChange }: {
  tabs: { key: string; label: string }[];
  active: string;
  onChange: (key: string) => void;
}) {
  return (
    <div className="tabs" role="tablist">
      {tabs.map((t) => (
        <button
          key={t.key}
          type="button"
          role="tab"
          aria-selected={t.key === active}
          className={t.key === active ? 'tab active' : 'tab'}
          onClick={() => onChange(t.key)}
        >
          {t.label}
        </button>
      ))}
    </div>
  );
}
