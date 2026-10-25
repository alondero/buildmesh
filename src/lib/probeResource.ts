import { useEffect, useMemo, useSyncExternalStore } from 'react';
import { formatError } from './errorUtils';

export interface ProbeLoadContext {
  meshId: number | null;
  meshPath?: string | null;
  activePath?: string | null;
  nodeId?: number | null;
  fallbackName?: string;
  filter?: 'open' | 'closed';
  force?: boolean;
}

export type ProbeLoader<T> = (context: ProbeLoadContext, signal: AbortSignal) => Promise<T>;

export interface ProbeResourceState<T> {
  data: T | null;
  loading: boolean;
  error: string | null;
  attempted: boolean;
  refreshing: boolean;
}

/**
 * One owner per scoped read. A new request aborts the old request; only the
 * newest live request may publish data, errors or completion. Reloads retain
 * this subject's data; changing subject creates an empty resource. A committed
 * mutation aborts older reads before updating data. IPC need not support abort:
 * the identity check still rejects its late completion, including rejection.
 */
export interface ProbeResourceOptions {
  pollIntervalMs?: number;
  refreshToken?: string;
  discardOnError?: boolean;
}

export function createProbeResource<T>(loader: ProbeLoader<T>, context: ProbeLoadContext, options: ProbeResourceOptions = {}) {
  const { pollIntervalMs, discardOnError = false } = options;
  let state: ProbeResourceState<T> = {
    data: null, loading: true, error: null, attempted: false, refreshing: false,
  };
  let active = false;
  let request: AbortController | null = null;
  let pollTimer: ReturnType<typeof setTimeout> | undefined;
  const listeners = new Set<() => void>();
  const publish = (next: Partial<ProbeResourceState<T>>) => {
    state = { ...state, ...next };
    for (const listener of listeners) listener();
  };
  const resource = {
    getSnapshot: () => state,
    subscribe(listener: () => void) {
      listeners.add(listener);
      return () => { listeners.delete(listener); };
    },
    activate() { active = true; },
    dispose() { active = false; request?.abort(); clearTimeout(pollTimer); },
    isActive: () => active,
    async reload(options: { force?: boolean; manual?: boolean } = {}) {
      if (!active) return false;
      clearTimeout(pollTimer);
      request?.abort();
      const owner = new AbortController();
      request = owner;
      const current = () => active && request === owner && !owner.signal.aborted;
      publish({ loading: true, error: null, refreshing: state.refreshing || !!options.manual });
      try {
        const data = await loader({ ...context, force: options.force ?? context.force }, owner.signal);
        if (!current()) return false;
        // A view may have reported an action failure after this read began.
        // Reload clears previous errors at entry; completion must retain newer ones.
        publish({ data, loading: false, attempted: true, refreshing: false });
        return true;
      } catch (cause) {
        if (!current()) return false;
        publish({
          ...(discardOnError ? { data: null } : {}),
          error: formatError(cause), loading: false, attempted: true, refreshing: false,
        });
        return false;
      } finally {
        if (current() && pollIntervalMs !== undefined) {
          pollTimer = setTimeout(() => void resource.reload(), pollIntervalMs);
        }
      }
    },
    invalidate() {
      if (!active) return;
      request?.abort();
      publish({ attempted: false, loading: true });
    },
    mutate(update: (data: T) => T) {
      if (!active || state.data === null) return;
      request?.abort();
      publish({ data: update(state.data), loading: false, refreshing: false });
    },
    reportError(error: string | null) {
      if (active) publish({ error });
    },
  };
  return resource;
}

export type ProbeResource<T> = ReturnType<typeof createProbeResource<T>>;

/** The key names the read's subject, including any server-side filter. */
export function useProbeResource<T>(loader: ProbeLoader<T>, context: ProbeLoadContext, key: string, options: ProbeResourceOptions = {}) {
  const { pollIntervalMs, refreshToken, discardOnError } = options;
  // The key is deliberately explicit: labels/fallbacks may change while an
  // editing session is open without authorizing another read over its draft.
  // eslint-disable-next-line react-hooks/exhaustive-deps -- context is captured only when its scoped key changes.
  const resource = useMemo(() => createProbeResource(loader, context, { pollIntervalMs, discardOnError }), [loader, key, pollIntervalMs, discardOnError]);
  const state = useSyncExternalStore(resource.subscribe, resource.getSnapshot);
  useEffect(() => {
    resource.activate();
    void resource.reload();
    return () => resource.dispose();
  }, [resource, refreshToken]);
  return { ...resource, state };
}
