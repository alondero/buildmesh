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
 *    `setData` / `setLoading` with `signal.aborted`, just like the
 *    mount-refetch effect. Without it, a `GIT_CHANGED` that fired in the
 *    gap between unsubscribe and remount could resolve a stale key's data
 *    into the new key's component.
 * 2. **Loading stuck on cache-hit** — the cache-hit short-circuit branch
 *    calls `setLoading(false)` explicitly. Otherwise, a previous render's
 *    `setLoading(true)` (e.g. a fetch was in flight when the user switched
 *    nodes) carries over into the new render and renders a perpetual
 *    spinner next to the cached value.
 */

import { useCallback, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import type { QueryClient } from '../lib/pathInvalidatedCache';
import { useAsyncEffect } from './useAsyncEffect';

export interface UsePathInvalidatedQueryResult<V> {
  data: V | null;
  loading: boolean;
  refresh: () => void;
}

export interface UsePathInvalidatedQueryOptions {
  /**
   * When `false`, the mount/key-change effect will NOT trigger an initial
   * fetch — useful for hooks that want to control when the first fetch
   * happens (e.g. `useMeshGitStatus` runs a static check first and only
   * fetches the file list once the path is known to be a git repo).
   *
   * The `GIT_CHANGED` subscription is unaffected — when `enabled: false`,
   * the hook still subscribes for invalidation, so a `GIT_CHANGED` event
   * after the first fetch will still trigger a re-fetch.
   *
   * Defaults to `true` to match the other three hooks' semantics.
   */
  enabled?: boolean;
  /**
   * When `true`, also refetch whenever the OS window regains focus — the user
   * may have run git in an external terminal while the app was unfocused.
   * Opt-in so consumers that don't want it are unaffected. The cache dedupes,
   * so a focus-driven refresh collapses onto any in-flight fetch for the key.
   *
   * Defaults to `false`.
   */
  refetchOnFocus?: boolean;
}

export function usePathInvalidatedQuery<K, V>(
  client: QueryClient<K, V>,
  key: K | null | undefined,
  path?: string | null,
  options: UsePathInvalidatedQueryOptions = {},
): UsePathInvalidatedQueryResult<V> {
  const { enabled = true, refetchOnFocus = false } = options;
  const [data, setData] = useState<V | null>(() => {
    // Seed from cache so a remount serves the cached value without a
    // re-fetch. Mirrors the four original hooks' `useState(() => ...)` pattern.
    if (key == null) return null;
    const cached = client.read(key);
    return cached === undefined ? null : cached;
  });
  // Match the original hooks' semantics: `loading` is `true` only when we
  // know we have to fetch (uncached). On a cache hit, we stay `false` so
  // consumers don't flash a spinner for a value we already have.
  const [loading, setLoading] = useState<boolean>(() => {
    if (key == null) return false;
    return client.read(key) === undefined;
  });

  // Mount / key change: read from cache, otherwise fetch (unless the
  // caller opted out via `enabled: false`). The signal protects against
  // the classic async-race where a key changes (or the component
  // unmounts) while a fetch is in flight — issue #349.
  useAsyncEffect((signal) => {
    if (key == null) {
      setData(null);
      setLoading(false);
      return;
    }

    if (!enabled) {
      // Caller is responsible for the initial fetch (typically via a
      // sibling effect that runs a precondition check first). Seed
      // loading from the cache so we don't show a perpetual spinner.
      if (signal.aborted) return;
      const cached = client.read(key);
      if (signal.aborted) return;
      setData(cached === undefined ? null : cached);
      setLoading(false);
      return;
    }

    const cached = client.read(key);
    if (cached !== undefined) {
      // Footgun 3: explicitly clear loading here. Otherwise a previous
      // mount's `setLoading(true)` carries over and renders a perpetual
      // spinner next to the cached value.
      setData(cached);
      setLoading(false);
      return;
    }

    setLoading(true);
    client.refresh(key).then((result) => {
      if (signal.aborted) return;
      setData(result);
      setLoading(false);
    });
  }, [key, client, enabled]);

  // Subscribe to GIT_CHANGED invalidation. The bus already evicted the
  // cache entry for `key` by the time `notify` fires, so this refresh hits
  // the backend (or dedups onto a sibling subscriber's in-flight fetch).
  useAsyncEffect((signal) => {
    if (key == null || !path) return;
    const unsubscribe = client.subscribe(key, path, () => {
      if (signal.aborted) return;
      setLoading(true);
      client.refresh(key).then((result) => {
        if (signal.aborted) return;
        setData(result);
        setLoading(false);
      });
    });
    return () => {
      unsubscribe();
    };
  }, [key, path, client]);

  // Optional: refetch when the window regains focus. Same signal guard as
  // the subscribe effect, so a focus event firing during a key change
  // can't resolve a stale key's data into the new render.
  useAsyncEffect((signal) => {
    if (key == null || !refetchOnFocus) return;
    let unlisten: (() => void) | null = null;
    getCurrentWindow()
      .onFocusChanged(({ payload: focused }) => {
        if (!focused || signal.aborted) return;
        setLoading(true);
        client.refresh(key).then((result) => {
          if (signal.aborted) return;
          setData(result);
          setLoading(false);
        });
      })
      .then((fn) => {
        if (signal.aborted) fn();
        else unlisten = fn;
      });
    return () => {
      if (unlisten) unlisten();
    };
  }, [key, client, refetchOnFocus]);

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
