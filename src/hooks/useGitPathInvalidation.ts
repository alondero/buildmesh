/**
 * React glue for the callback-only path-invalidation subscription
 * (`subscribeGitPathInvalidation` in `lib/pathInvalidatedCache.ts`).
 *
 * Why this hook exists (issue #357)
 * ----------------------------------
 * The hand-rolled `useEffect(() => subscribeGitPathInvalidation(path, cb),
 * [path, cb])` pattern has a subtle race: when `path` changes mid-mount
 * (e.g. user switches nodes), the effect cleanup unsubscribes the old
 * path and re-subscribes the new one. If a `GIT_CHANGED` event fires in
 * the gap, the OLD `cb` runs against the NEW component. The hook side
 * (`usePathInvalidatedQuery`) already fixes this with the AbortSignal
 * passed by `useAsyncEffect` (issue #349) that the bus callback
 * short-circuits on. The components that used `subscribeGitPathInvalidation`
 * directly (`AgentReviewPanel`, `ChangedFilesPanel`) had the same
 * pre-existing footgun.
 *
 * Centralising the fix here means a future callback-only consumer can't
 * accidentally re-introduce the race. The primitive stays as a low-level
 * escape hatch for non-React code.
 *
 * Usage
 * -----
 *   useGitPathInvalidation(rootPath, () => fetchDiff({ background: true }));
 *
 * `cb` is read via a ref (the `useEffectEvent` pattern) so an inline
 * arrow is safe — the effect only re-subscribes on `path` change, NOT
 * every render. This matters because both call sites pass inline
 * arrows, and listing `cb` in the deps array would tear down and
 * rebuild the global bus subscription on every parent re-render.
 */

import { useRef } from 'react';
import { subscribeGitPathInvalidation } from '../lib/pathInvalidatedCache';
import { useAsyncEffect } from './useAsyncEffect';

export function useGitPathInvalidation(
  path: string | null | undefined,
  cb: () => void,
): void {
  // Read the latest callback through a ref so the subscribe effect can
  // depend on `[path]` only. Without this, an inline arrow at the call
  // site would be a new function reference every render, and the hook
  // would unsubscribe and re-subscribe on every parent re-render
  // (mutating module-level bus state on every state change). With the
  // ref, the effect re-runs only when `path` actually changes.
  const cbRef = useRef(cb);
  cbRef.current = cb;

  useAsyncEffect((signal) => {
    if (!path) return;
    const unsubscribe = subscribeGitPathInvalidation(path, () => {
      if (signal.aborted) return;
      cbRef.current();
    });
    return () => {
      unsubscribe();
    };
  }, [path]);
}
