/**
 * React glue for the path-invalidated cache primitive
 * (`pathInvalidatedCache.ts`, issue #282, split into two factories in
 * issue #347).
 *
 * Overloads
 * ---------
 * The hook accepts BOTH client shapes:
 *   - `usePathInvalidatedQuery(client, key)` — `client` is a
 *     `PathKeyedClient<V>`; `key` doubles as the GIT_CHANGED subscription
 *     path. Used by hooks whose key IS a path: `useGitSummary`,
 *     `useMeshGitStatus`, `useGitBranchStatus`, `useChangedFiles`.
 *   - `usePathInvalidatedQuery(client, key, path)` — `client` is a
 *     `DualKeyClient<K, V>`; `key` is the entity id and `path` is the
 *     separate GIT_CHANGED subscription path. Used by `useOpenPr`,
 *     `useMeshHealth`.
 *
 * The TypeScript compiler picks the right overload from the client type.
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
import type { DualKeyClient, PathKeyedClient } from '../lib/pathInvalidatedCache';
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
   * Defaults to `true` to match the other hooks' semantics.
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

// ------------------------------------------------------------------
// Public overloads — TypeScript picks one based on the client type.
// ------------------------------------------------------------------

/** Single-key shape: `client` is a `PathKeyedClient<V>`; `key` doubles as
 * the GIT_CHANGED subscription path. */
export function usePathInvalidatedQuery<V>(
  client: PathKeyedClient<V>,
  key: string | null | undefined,
  options?: UsePathInvalidatedQueryOptions,
): UsePathInvalidatedQueryResult<V>;

/** Dual-key shape: `client` is a `DualKeyClient<K, V>`; `key` is the
 * entity id and `path` is the separate GIT_CHANGED subscription path.
 *
 * `path` is `string | null` (NOT `string | null | undefined`) — passing
 * `undefined` would be ambiguous with the single-key form's "no options"
 * slot, and the impl's `isDualKey` discriminator relies on this distinction
 * (string|null = dual-key, object|undefined = single-key). A caller that
 * wants to skip the GIT_CHANGED subscription on a dual-key client passes
 * `null`. All current callers (`useOpenPr`, `useMeshHealth`) type `path`
 * as `string | null`, so this is the natural fit. */
export function usePathInvalidatedQuery<K, V>(
  client: DualKeyClient<K, V>,
  key: K | null | undefined,
  path: string | null,
  options?: UsePathInvalidatedQueryOptions,
): UsePathInvalidatedQueryResult<V>;

/** Implementation signature. The overloads above give the public API its
 * shape; the impl accepts either client and dispatches to the right
 * `subscribe*` method. */
export function usePathInvalidatedQuery(
  client: PathKeyedClient<unknown> | DualKeyClient<unknown, unknown>,
  key: unknown,
  // `undefined` is intentionally NOT allowed for the dual-key shape
  // (see the dual-key overload's docstring above — passing `undefined`
  // would be ambiguous with "no options" on the single-key form).
  // The single-key shape uses `undefined` to mean "no options"; the
  // dual-key shape requires an explicit `null` to skip the subscription.
  pathOrOptions?: string | null | UsePathInvalidatedQueryOptions,
  options: UsePathInvalidatedQueryOptions = {},
): UsePathInvalidatedQueryResult<unknown> {
  // Discriminate the overload: the dual-key form puts a string-or-null
  // `path` in the 3rd position; the single-key form puts the options
  // object there (or nothing, when options are omitted).
  const isDualKey = typeof pathOrOptions === 'string' || pathOrOptions === null;
  const path = (isDualKey ? pathOrOptions : undefined) as string | null | undefined;
  const opts: UsePathInvalidatedQueryOptions = isDualKey
    ? options
    : (pathOrOptions ?? options ?? {});

  const { enabled = true, refetchOnFocus = false } = opts;
  const [data, setData] = useState<unknown>(() => {
    // Seed from cache so a remount serves the cached value without a
    // re-fetch. Mirrors the four original hooks' `useState(() => ...)` pattern.
    if (key == null) return null;
    const cached = client.read(key as never);
    return cached === undefined ? null : cached;
  });
  // Match the original hooks' semantics: `loading` is `true` only when we
  // know we have to fetch (uncached). On a cache hit, we stay `false` so
  // consumers don't flash a spinner for a value we already have.
  const [loading, setLoading] = useState<boolean>(() => {
    if (key == null) return false;
    return client.read(key as never) === undefined;
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
      const cached = client.read(key as never);
      if (signal.aborted) return;
      setData(cached === undefined ? null : cached);
      setLoading(false);
      return;
    }

    const cached = client.read(key as never);
    if (cached !== undefined) {
      // Footgun 3: explicitly clear loading here. Otherwise a previous
      // mount's `setLoading(true)` carries over and renders a perpetual
      // spinner next to the cached value.
      setData(cached);
      setLoading(false);
      return;
    }

    setLoading(true);
    client.refresh(key as never).then((result) => {
      if (signal.aborted) return;
      setData(result);
      setLoading(false);
    });
  }, [key, client, enabled]);

  // Subscribe to GIT_CHANGED invalidation. The bus already evicted the
  // cache entry for `key` by the time `notify` fires, so this refresh hits
  // the backend (or dedups onto a sibling subscriber's in-flight fetch).
  // Single-key clients call `subscribe(key, cb)`; dual-key clients call
  // `subscribeByPath(key, path, cb)`.
  useAsyncEffect((signal) => {
    if (key == null) return;
    if (isDualKey && !path) return;  // dual-key callers must opt in with a path
    const onInvalidate = () => {
      if (signal.aborted) return;
      setLoading(true);
      client.refresh(key as never).then((result) => {
        if (signal.aborted) return;
        setData(result);
        setLoading(false);
      });
    };
    const unsubscribe = isDualKey
      ? (client as DualKeyClient<unknown, unknown>).subscribeByPath(key, path as string, onInvalidate)
      : (client as PathKeyedClient<unknown>).subscribe(key as string, onInvalidate);
    return () => {
      unsubscribe();
    };
  }, [key, path, client, isDualKey]);

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
        client.refresh(key as never).then((result) => {
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
    client.invalidate(key as never);
    setLoading(true);
    client.refresh(key as never).then((result) => {
      setData(result);
      setLoading(false);
    });
  }, [key, client]);

  return { data, loading, refresh };
}
