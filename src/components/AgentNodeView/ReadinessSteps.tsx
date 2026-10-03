import { useEffect, useRef, useState } from 'react';
import type { CanvasEmptyStateCallbacks } from './CanvasEmptyState';
import { retryProviderList, useProviderReadiness } from '../../hooks/useProviderList';

const STORAGE_KEY = 'buildmesh.readiness-dismissed';
export function ReadinessSteps({ repositoryReady, harnessReady, callbacks }: {
  repositoryReady: boolean; harnessReady: boolean; callbacks: CanvasEmptyStateCallbacks;
}) {
  const readiness = useProviderReadiness();
  const [starting, setStarting] = useState(false);
  const inFlight = useRef(false);
  const mounted = useRef(true);
  useEffect(() => { mounted.current = true; return () => { mounted.current = false; }; }, []);
  async function startTerminal() {
    if (inFlight.current) return;
    inFlight.current = true; setStarting(true);
    try { await callbacks.onOpenTerminal?.(); }
    finally { inFlight.current = false; if (mounted.current) setStarting(false); }
  }
  const [dismissed, setDismissed] = useState(() => {
    try { return localStorage.getItem(STORAGE_KEY) === 'true'; } catch { return false; }
  });
  const actionClass = 'min-h-[24px] rounded-md px-2 text-xs text-accent-cyan hover:bg-bg-card-hover';
  return <div className="mt-5 text-left text-xs text-text-secondary">
    {!dismissed && <ol aria-label="Getting started" className="space-y-3 rounded-md border border-border-default bg-bg-surface p-3">
      <li><span className="font-medium text-text-primary">1. Choose a repository</span>
        <p>{repositoryReady ? 'Repository added. Ready for a session.' : 'Add a mesh pointing to your local Git repository.'}</p>
      </li>
      <li><span className="font-medium text-text-primary">2. Check your agent setup</span>
        <p>{harnessReady ? 'A harness is available. Confirm its runtime and login before launching.' : 'Install or enable a harness and check its runtime and login in Settings.'} A Terminal works without an agent login.</p>
        <button type="button" onClick={callbacks.onOpenSetup} className={actionClass}>Check runtime and login</button>
        {readiness.status === 'loading' && <p role="status">Checking available harnesses… You can start a Terminal while checks run.</p>}
        {readiness.status === 'failed' && <div role="alert" className="break-all text-status-error">Could not check harnesses: {readiness.error}<button type="button" onClick={retryProviderList} className={actionClass}>Retry harness check</button></div>}
      </li>
      <li><span className="font-medium text-text-primary">3. Start a session</span>
        <p>{repositoryReady ? 'Open a Terminal, or start your first agent.' : 'Add a repository first, then open a Terminal or agent.'}</p>
        {repositoryReady && callbacks.onOpenTerminal && <button type="button" disabled={starting} onClick={() => void startTerminal()} className={actionClass}>{starting ? 'Starting Terminal…' : 'Start Terminal'}</button>}
      </li>
    </ol>}
    <div className="mt-2 flex justify-center gap-2">
      <button type="button" className={actionClass} onClick={() => {
        const next = !dismissed;
        setDismissed(next);
        try { localStorage.setItem(STORAGE_KEY, String(next)); } catch { /* Session-only when storage is unavailable. */ }
      }}>{dismissed ? 'Show setup guide' : 'Skip setup guide'}</button>
      {callbacks.onOpenHelp && <button type="button" onClick={callbacks.onOpenHelp} className={actionClass}>Help and shortcuts</button>}
    </div>
    {dismissed && repositoryReady && callbacks.onOpenTerminal && <button type="button" disabled={starting} onClick={() => void startTerminal()} className={actionClass}>{starting ? 'Starting Terminal…' : 'Start Terminal'}</button>}
  </div>;
}
