import { useCallback, useEffect, useState } from 'react';
import { api } from '../api';

/**
 * GET a JSON endpoint: `const {data, error, loading, reload} = useApi<T>('/queue')`.
 * Pass `null` to skip fetching. `reload()` re-fetches. Aborts on unmount.
 */
export function useApi<T>(path: string | null) {
  const [data, setData] = useState<T | null>(null);
  const [error, setError] = useState<unknown>(null);
  const [loading, setLoading] = useState(path !== null);
  const [tick, setTick] = useState(0);

  useEffect(() => {
    if (path === null) {
      setData(null);
      setLoading(false);
      return;
    }
    const ac = new AbortController();
    setLoading(true);
    setError(null);
    api<T>('GET', path, undefined, { signal: ac.signal })
      .then(setData)
      .catch((e) => {
        if (!ac.signal.aborted) setError(e);
      })
      .finally(() => {
        if (!ac.signal.aborted) setLoading(false);
      });
    return () => ac.abort();
  }, [path, tick]);

  const reload = useCallback(() => setTick((t) => t + 1), []);
  return { data, error, loading, reload };
}
