import { useEffect, useRef, useState } from 'react';
import type { CanvasEmptyStateCallbacks } from './CanvasEmptyState';
import { retryProviderList, useProviderReadiness } from '../../hooks/useProviderList';

/** Setup status and the terminal fallback share the empty state's scroll owner. */
export function EmptyStateActions({ repositoryReady, harnessReady, callbacks }: {
  repositoryReady: boolean;
  harnessReady: boolean;
  callbacks: CanvasEmptyStateCallbacks;
}) {
  const readiness = useProviderReadiness();
  const [starting, setStarting] = useState(false);
  const inFlight = useRef(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  async function startTerminal() {
    if (inFlight.current) return;
    inFlight.current = true;
    setStarting(true);
    try {
      await callbacks.onOpenTerminal?.();
    } finally {
      inFlight.current = false;
      if (mounted.current) setStarting(false);
    }
  }

  const actionClass = 'min-h-[24px] rounded-md px-2 text-xs text-text-secondary transition-colors hover:bg-bg-card-hover hover:text-text-primary';
  return (
    <div className="mt-3 space-y-3 text-xs text-text-secondary">
      <div className="flex flex-wrap justify-center gap-2">
        {repositoryReady && callbacks.onOpenTerminal && (
          <button type="button" disabled={starting} onClick={() => void startTerminal()} className={actionClass}>
            {starting ? 'Starting Terminal…' : 'Start Terminal'}
          </button>
        )}
        {!repositoryReady && !harnessReady && (
          <button type="button" onClick={callbacks.onOpenSetup} className={actionClass}>Agent settings</button>
        )}
      </div>
      {readiness.status === 'loading' && <p role="status">Checking available harnesses…</p>}
      {readiness.status === 'failed' && (
        <div role="alert" className="break-words text-status-error [overflow-wrap:anywhere]">
          <p>Could not check harnesses: {readiness.error}</p>
          <button type="button" onClick={retryProviderList} className={actionClass}>Retry harness check</button>
        </div>
      )}
    </div>
  );
}
