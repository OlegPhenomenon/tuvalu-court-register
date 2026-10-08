import type { ReactNode } from 'react';

export interface Column<T> {
  /** Key of the row object — also used as the fallback cell text. */
  key: Extract<keyof T, string>;
  header: ReactNode;
  /** Custom cell renderer; defaults to `String(row[key] ?? '')`. */
  render?: (row: T) => ReactNode;
}

/**
 * Accessible data table: `<DataTable columns rows empty="Nothing here" rowKey={r=>r.id} />`.
 * `empty` is shown as a single spanning row when `rows` is empty.
 */
export function DataTable<T>({ columns, rows, empty, rowKey }: {
  columns: Column<T>[];
  rows: T[];
  empty: ReactNode;
  rowKey?: (row: T) => string;
}) {
  return (
    <div className="table-wrap">
      <table>
        <thead>
          <tr>
            {columns.map((c) => <th key={c.key} scope="col">{c.header}</th>)}
          </tr>
        </thead>
        <tbody>
          {rows.length === 0 ? (
            <tr><td className="table-empty" colSpan={columns.length}>{empty}</td></tr>
          ) : rows.map((row, i) => (
            <tr key={rowKey ? rowKey(row) : i}>
              {columns.map((c) => (
                <td key={c.key}>
                  {c.render ? c.render(row) : String(row[c.key] ?? '')}
                </td>
              ))}
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}
