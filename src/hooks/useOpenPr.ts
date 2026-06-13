import { useEffect, useState, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getOpenPrForNode, type OpenPr } from '../lib/tauri';
import { GIT_CHANGED } from '../lib/events';
import { pathMatchesGitEvent } from '../lib/paths';

// Module-level shared cache (keyed by nodeId). null is a real state
// ("no open PR"), so we use `has` to disambiguate "uncached" from "cached null".
const cache = new Map<number, OpenPr | null>();

// In-flight fetches deduped per nodeId so multiple subscribers don't duplicate
// the network call.
const pendingFetches = new Map<number, Promise<OpenPr | null>>();

// Subscribers keyed by git path (because `GIT_CHANGED` carries a path).
// Each subscriber records its `nodeId` so the listener can invalidate the
// node-keyed cache before notifying — without the invalidation, `fetchPr`
// would serve the stale cache entry and the chip would never refresh.
interface PathSubscriber {
  nodeId: number;
  notify: () => void;
}

const pathSubscribers = new Map<string, Set<PathSubscriber>>();

let listenerInstalled = false;

function installListener() {
  if (listenerInstalled) return;
  listenerInstalled = true;

  listen(GIT_CHANGED, (event) => {
    const payload = event.payload as { path: string; internal_path?: string };
    // Issue #304: same pattern as useGitSummary — iterate all path-keyed
    // subscribers and use the shared helper. The previous
    // `[path, internal_path].filter(Boolean)` lookup missed the WSL UNC
    // slash-mismatch case and any future worktree-subdir events.
    for (const [key, subs] of pathSubscribers) {
      if (!pathMatchesGitEvent(payload, key)) continue;
      // Invalidate every affected node first, then notify. The first notify
      // starts a fetch (registering a pendingFetch); later subscribers of the
      // same node then dedup onto it instead of refetching.
      subs.forEach(s => {
        cache.delete(s.nodeId);
        pendingFetches.delete(s.nodeId);
      });
      subs.forEach(s => s.notify());
    }
  });
}

function subscribeByPath(path: string, subscriber: PathSubscriber): () => void {
  if (!pathSubscribers.has(path)) pathSubscribers.set(path, new Set());
  pathSubscribers.get(path)!.add(subscriber);
  return () => {
    const subs = pathSubscribers.get(path);
    if (subs) {
      subs.delete(subscriber);
      if (subs.size === 0) pathSubscribers.delete(path);
    }
  };
}

/**
 * Hook that fetches the open PR for an agent node's branch, if any.
 *
 * Returns `null` for the common cases (no PR, no auth, not a git repo, branch
 * unborn, non-GitHub origin) — the chip is hidden whenever `pr === null`.
 *
 * Cached per nodeId; auto-refetched on `GIT_CHANGED` for the node's git path.
 * The `refresh()` handle is exposed for manual invalidation (e.g. after a
 * PR is created/merged via buildmesh's own flow — wiring that up is a v1.1).
 */
export function useOpenPr(nodeId: number, gitPath: string | null): {
  pr: OpenPr | null;
  loading: boolean;
  refresh: () => void;
} {
  const [pr, setPr] = useState<OpenPr | null>(() => {
    // Initial seed reads the cache directly so a remount doesn't re-hit the API.
    return cache.has(nodeId) ? cache.get(nodeId)! : null;
  });
  const [loading, setLoading] = useState(false);

  const fetchPr = useCallback((id: number) => {
    if (cache.has(id)) {
      setPr(cache.get(id)!);
      return;
    }

    const pending = pendingFetches.get(id);
    if (pending) {
      setLoading(true);
      pending.then(result => {
        setPr(result);
        setLoading(false);
      });
      return;
    }

    setLoading(true);
    const p = getOpenPrForNode(id)
      .then(result => {
        cache.set(id, result);
        pendingFetches.delete(id);
        setPr(result);
        setLoading(false);
        return result;
      })
      .catch(err => {
        // Network / rate-limit / 5xx — drop the in-flight, leave the cache
        // untouched, and let the next `GIT_CHANGED` try again.
        pendingFetches.delete(id);
        setLoading(false);
        // Surface to the dev console without breaking the UI.
        // eslint-disable-next-line no-console
        console.warn('useOpenPr: fetch failed for node', id, err);
        return null;
      });
    pendingFetches.set(id, p);
  }, []);

  // Install the global GIT_CHANGED listener once per process.
  useEffect(() => {
    installListener();
  }, []);

  // Fetch on mount / nodeId change.
  useEffect(() => {
    if (!nodeId) {
      setPr(null);
      return;
    }
    fetchPr(nodeId);
  }, [nodeId, fetchPr]);

  // Subscribe to invalidation events for this node's git path. The listener
  // has already evicted our cache entry by the time notify fires, so this
  // fetch hits the backend (or dedups onto a sibling subscriber's fetch).
  useEffect(() => {
    if (!gitPath || !nodeId) return;
    const myId = nodeId;
    const unsubscribe = subscribeByPath(gitPath, {
      nodeId: myId,
      notify: () => fetchPr(myId),
    });
    return unsubscribe;
  }, [gitPath, nodeId, fetchPr]);

  const refresh = useCallback(() => {
    if (!nodeId) return;
    cache.delete(nodeId);
    pendingFetches.delete(nodeId);
    fetchPr(nodeId);
  }, [nodeId, fetchPr]);

  return { pr, loading, refresh };
}
