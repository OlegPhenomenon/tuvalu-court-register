import { useId } from 'react';
import { Link } from 'react-router-dom';

/**
 * "What happens next" box for `next_actions` from GET /api/cases/:id:
 * `<NextActions items={[{code, message, link?}]} />`. Renders nothing when empty.
 */
export function NextActions({ items }: { items: { code: string; message: string; link?: string }[] }) {
  const id = useId();
  if (items.length === 0) return null;
  return (
    <section className="next-actions" aria-labelledby={id}>
      <h2 id={id}>What happens next</h2>
      <ul>
        {items.map((item) => (
          <li key={`${item.code}:${item.message}`}>
            {item.link ? <Link to={item.link}>{item.message}</Link> : item.message}
          </li>
        ))}
      </ul>
    </section>
  );
}
