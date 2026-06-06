import { useEffect, useState, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getOpenPrForNode, type OpenPr } from '../lib/tauri';
import { GIT_CHANGED } from '../lib/events';

// Module-level shared cache (keyed by nodeId). null is a real state
// ("no open PR"), so we use `has` to disambiguate "uncached" from "cached null".
const cache = new Map<number, OpenPr | null>();

// In-flight fetches deduped per nodeId so multiple subscribers don't duplicate
// the network call.
const pendingFetches = new Map<number, Promise<OpenPr | null>>();

// Subscribers keyed by git path (because `GIT_CHANGED` carries a path).
// Each callback closes over its own `nodeId` and compares on fire; the sentinel
// `0` means "broadcast" — every subscriber re-checks, since the path→node
// mapping isn't knowable here. Cache dedup keeps this cheap.
const pathSubscribers = new Map<string, Set<(nodeId: number) => void>>();

let listenerInstalled = false;

function installListener() {
  if (listenerInstalled) return;
  listenerInstalled = true;

  listen(GIT_CHANGED, (event) => {
    const { path, internal_path } = event.payload as { path: string; internal_path?: string };
    [path, internal_path].filter(Boolean).forEach(p => {
      const subs = pathSubscribers.get(p!);
      if (subs) subs.forEach(cb => cb(0)); // 0 = broadcast
    });
  });
}

function subscribeByPath(path: string, cb: (nodeId: number) => void): () => void {
  if (!pathSubscribers.has(path)) pathSubscribers.set(path, new Set());
  pathSubscribers.get(path)!.add(cb);
  return () => {
    const subs = pathSubscribers.get(path);
    if (subs) {
      subs.delete(cb);
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

  // Subscribe to invalidation events for this node's git path.
  // The callback only refetches when the fired nodeId matches our own —
  // otherwise the broadcast is a no-op for us.
  useEffect(() => {
    if (!gitPath || !nodeId) return;
    const myId = nodeId;
    const unsubscribe = subscribeByPath(gitPath, (changedId) => {
      if (changedId === 0 || changedId === myId) {
        fetchPr(myId);
      }
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
