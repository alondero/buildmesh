import { useEffect, useState, useCallback } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getGitSummary, type GitSummary } from '../lib/tauri';
import { GIT_CHANGED } from '../lib/events';

// Module-level shared cache (keyed by git path)
const cache = new Map<string, GitSummary>();
const pendingFetches = new Map<string, Promise<GitSummary | null>>();

// Subscribers keyed by path — notified when cache is invalidated
const subscribers = new Map<string, Set<() => void>>();

// Global listener — installed once
let listenerInstalled = false;

function installListener() {
  if (listenerInstalled) return;
  listenerInstalled = true;

  listen(GIT_CHANGED, (event) => {
    const { path, internal_path } = event.payload as { path: string; internal_path?: string };
    // Invalidate both the host path (UNC) and internal path (Linux) for this watched directory
    cache.delete(path);
    if (internal_path) cache.delete(internal_path);
    pendingFetches.delete(path);
    if (internal_path) pendingFetches.delete(internal_path);
    // Notify all subscribers watching either path
    [path, internal_path].filter(Boolean).forEach(p => {
      const subs = subscribers.get(p!);
      if (subs) {
        subs.forEach(cb => cb());
      }
    });
  });
}

function subscribe(path: string, cb: () => void) {
  if (!subscribers.has(path)) {
    subscribers.set(path, new Set());
  }
  subscribers.get(path)!.add(cb);
  return () => {
    const subs = subscribers.get(path);
    if (subs) {
      subs.delete(cb);
      if (subs.size === 0) subscribers.delete(path);
    }
  };
}

/**
 * Hook that fetches and caches git summary for a given path.
 * Automatically refetches when a GIT_CHANGED event fires for that path.
 */
export function useGitSummary(gitPath: string | null): {
  summary: GitSummary | null;
  loading: boolean;
  refresh: () => void;
} {
  const [summary, setSummary] = useState<GitSummary | null>(() => {
    if (!gitPath) return null;
    const cached = cache.get(gitPath);
    return cached && cached.total > 0 ? cached : null;
  });
  const [loading, setLoading] = useState(false);

  const fetchSummary = useCallback((path: string) => {
    // Check cache first
    const cached = cache.get(path);
    if (cached) {
      setSummary(cached.total > 0 ? cached : null);
      return;
    }

    // Check if a fetch is already in flight
    const pending = pendingFetches.get(path);
    if (pending) {
      setLoading(true);
      pending.then(result => {
        if (result) {
          setSummary(result.total > 0 ? result : null);
        }
        setLoading(false);
      });
      return;
    }

    // Start a new fetch
    setLoading(true);
    const p = getGitSummary(path)
      .then(result => {
        cache.set(path, result);
        pendingFetches.delete(path);
        setSummary(result.total > 0 ? result : null);
        setLoading(false);
        return result;
      })
      .catch(() => {
        pendingFetches.delete(path);
        setLoading(false);
        return null;
      });

    pendingFetches.set(path, p);
  }, []);

  // Install the global GIT_CHANGED listener
  useEffect(() => {
    installListener();
  }, []);

  // Fetch on mount / path change
  useEffect(() => {
    if (!gitPath) {
      setSummary(null);
      return;
    }
    fetchSummary(gitPath);
  }, [gitPath, fetchSummary]);

  // Subscribe to invalidation events for this path
  useEffect(() => {
    if (!gitPath) return;

    const unsubscribe = subscribe(gitPath, () => {
      fetchSummary(gitPath);
    });

    return unsubscribe;
  }, [gitPath, fetchSummary]);

  const refresh = useCallback(() => {
    if (!gitPath) return;
    cache.delete(gitPath);
    pendingFetches.delete(gitPath);
    fetchSummary(gitPath);
  }, [gitPath, fetchSummary]);

  return { summary, loading, refresh };
}
