/**
 * React glue for the path-invalidated cache primitive (`pathInvalidatedCache.ts`).
 *
 * - `key` — the cache key. Pass `null`/`undefined` to disable fetching.
 * - `path` — the path to subscribe to for `GIT_CHANGED` invalidation. When
 *   omitted (or `null`), the hook still reads/refreshes from cache but does
 *   not subscribe to bus events. Useful for hooks that want a manual
 *   `refresh()` only.
 *
 * Footguns already-fixed (do not re-introduce):
 * 1. **Subscribe `onInvalidate` race** — the subscribe effect guards its
 *    `setData` / `setLoading` with a `cancelled` flag, just like the
 *    mount-refetch effect. Without it, a `GIT_CHANGED` that fired in the
 *    gap between unsubscribe and remount could resolve a stale key's data
 *    into the new key's component.
 * 2. **Loading stuck on cache-hit** — the cache-hit short-circuit branch
 *    calls `setLoading(false)` explicitly. Otherwise, a previous render's
 *    `setLoading(true)` (e.g. a fetch was in flight when the user switched
 *    nodes) carries over into the new render and renders a perpetual
 *    spinner next to the cached value.
 */

import { useCallback, useEffect, useState } from 'react';
import type { QueryClient } from '../lib/pathInvalidatedCache';

export interface UsePathInvalidatedQueryResult<V> {
  data: V | null;
  loading: boolean;
  refresh: () => void;
}

export function usePathInvalidatedQuery<K, V>(
  client: QueryClient<K, V>,
  key: K | null | undefined,
  path?: string | null,
): UsePathInvalidatedQueryResult<V> {
  const [data, setData] = useState<V | null>(() => {
    // Seed from cache so a remount serves the cached value without a
    // re-fetch. Mirrors the four original hooks' `useState(() => ...)` pattern.
    if (key == null) return null;
    const cached = client.read(key);
    return cached === undefined ? null : cached;
  });
  const [loading, setLoading] = useState<boolean>(key != null);

  // Mount / key change: read from cache, otherwise fetch. The cancelled
  // flag protects against the classic async-race where a key changes (or
  // the component unmounts) while a fetch is in flight.
  useEffect(() => {
    let cancelled = false;

    if (key == null) {
      setData(null);
      setLoading(false);
      return () => {
        cancelled = true;
      };
    }

    const cached = client.read(key);
    if (cached !== undefined) {
      // Footgun 3: explicitly clear loading here. Otherwise a previous
      // mount's `setLoading(true)` carries over and renders a perpetual
      // spinner next to the cached value.
      setData(cached);
      setLoading(false);
      return () => {
        cancelled = true;
      };
    }

    setLoading(true);
    client.refresh(key).then((result) => {
      if (cancelled) return;
      setData(result);
      setLoading(false);
    });

    return () => {
      cancelled = true;
    };
  }, [key, client]);

  // Subscribe to GIT_CHANGED invalidation. The bus already evicted the
  // cache entry for `key` by the time `notify` fires, so this refresh hits
  // the backend (or dedups onto a sibling subscriber's in-flight fetch).
  useEffect(() => {
    if (key == null || !path) return;
    let cancelled = false;
    const unsubscribe = client.subscribe(key, path, () => {
      if (cancelled) return;
      setLoading(true);
      client.refresh(key).then((result) => {
        if (cancelled) return;
        setData(result);
        setLoading(false);
      });
    });
    return () => {
      cancelled = true;
      unsubscribe();
    };
  }, [key, path, client]);

  const refresh = useCallback(() => {
    if (key == null) return;
    client.invalidate(key);
    setLoading(true);
    client.refresh(key).then((result) => {
      setData(result);
      setLoading(false);
    });
  }, [key, client]);

  return { data, loading, refresh };
}
