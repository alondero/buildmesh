/**
 * GitIssuesTab — the Probe Panel's 🐙 tab body (issue #378).
 *
 * Thin wrapper port of the legacy `GitHubIssuesModal` (issue #113). The
 * dock supplies the header, mesh-name subheading, and close button, so
 * this component drops the modal's backdrop / header / Escape handler
 * and renders the same list + split-spawn button in the probe's 360px
 * body.
 *
 * Two-stage spawn (issue #302)
 * ----------------------------
 * The primary "Spawn" button uses the mesh's resolved default provider
 * (explicit > per-mesh > app-wide > "anthropic" fallback, enforced
 * server-side by `resolve_default_provider`). The `▾` half of the split
 * button opens a provider picker that bypasses the default. Both call
 * sites go through the same two-stage flow:
 *
 *   1. `create_issue_node` — fast DB-only IPC (~20ms) returns a `pending`
 *      node + the prefill to hand off to stage 2.
 *   2. `start_node_background` — fire-and-forget; the slow work (git
 *      fetch, worktree create, PTY spawn) runs in the background, and
 *      `node-spawn-completed` / `node-spawn-failed` store listeners flip
 *      the node to `running` / `error`.
 *
 * The user sees the dock-close → node-appear transition in well under
 * 500ms instead of the 5-10s they used to wait for the old synchronous
 * `spawn_issue_agent`.
 */

import { useState, useEffect } from 'react';
import {
  getRepoIssues,
  createIssueNode,
  startNodeBackground,
  listProviders,
  type GitHubIssue,
} from '../../lib/tauri';
import { useMeshStore } from '../../stores/meshStore';
import { useProbeContext } from '../../hooks/useProbeContext';
import { ProviderDropdown, colorClassForProvider, type ProviderEntry } from '../Sidebar/ProviderDropdown';

export function GitIssuesTab() {
  const { activeMeshId } = useProbeContext();
  // `getDefaultProvider` is mesh-scoped — the only call that needs the
  // meshId directly, since it resolves the per-mesh > app-wide > default
  // precedence chain server-side.
  const getDefaultProvider = useMeshStore((s) => s.getDefaultProvider);

  const [issues, setIssues] = useState<GitHubIssue[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [spawning, setSpawning] = useState<number | null>(null);
  // Only one dropdown open at a time, keyed by issue number — mirrors the
  // SessionBrowserModal pattern so the click-outside handling stays simple.
  const [openDropdown, setOpenDropdown] = useState<number | null>(null);
  // The provider list is fetched once at mount and reused for the lifetime
  // of the tab. Re-fetching on each open would be wasteful, and the list is
  // stable for the duration of a session (adding a new provider requires
  // an app restart).
  const [providerList, setProviderList] = useState<ProviderEntry[]>([]);

  useEffect(() => {
    if (activeMeshId === null) return;
    let cancelled = false;
    const load = async () => {
      try {
        const result = await getRepoIssues(activeMeshId);
        // The mesh could have changed between opening the modal and the
        // IPC returning — drop the result in that case rather than
        // showing issues for a mesh the user no longer has focused.
        if (cancelled) return;
        setIssues(result);
      } catch (e) {
        if (cancelled) return;
        console.error('Failed to load issues:', e);
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        if (!cancelled) setLoading(false);
      }
    };
    setLoading(true);
    setError(null);
    load();
    return () => {
      cancelled = true;
    };
  }, [activeMeshId]);

  // Fetch the provider list once at mount. Platform filtering (e.g.
  // macOS-only Anthropic) is enforced server-side via
  // AgentProvider::available_on(). Re-fetching on every render would
  // be wasteful, and the list is stable for the lifetime of a session.
  useEffect(() => {
    let cancelled = false;
    listProviders()
      .then(backendProviders => {
        if (cancelled) return;
        setProviderList(
          backendProviders.map(p => ({ id: p.id, label: p.label, color: colorClassForProvider(p.id) })),
        );
      })
      .catch(err => console.error('listProviders failed:', err));
    return () => {
      cancelled = true;
    };
  }, []);

  // Close the provider dropdown when clicking outside of it. The dropdown
  // container carries a `data-dropdown-for` attribute set to the issue number.
  useEffect(() => {
    if (openDropdown === null) return;
    const handleClick = (e: MouseEvent) => {
      const target = e.target as HTMLElement;
      if (!target.closest(`[data-dropdown-for="${openDropdown}"]`)) {
        setOpenDropdown(null);
      }
    };
    document.addEventListener('mousedown', handleClick);
    return () => document.removeEventListener('mousedown', handleClick);
  }, [openDropdown]);

  // Two-stage spawn: stage-1 (`create_issue_node`) is a fast DB-only
  // IPC (~20ms) that returns a `pending` node + the prefill to hand
  // off to stage-2. The dock stays open after a successful spawn so
  // the user can fire off another issue without re-opening the
  // context-menu → "Open Issues" route — the legacy modal's
  // `onClose()` parity was a vestige from the one-shot dialog and
  // doesn't fit a persistent dock. Stage-2 (`startNodeBackground`)
  // is fire-and-forget — the slow work (git fetch, worktree create,
  // PTY spawn) runs in the background, and the node flips to
  // 'running' (or 'error' on failure) via the `node-spawn-completed`
  // / `node-spawn-failed` store listeners. The user dismisses the
  // dock with the activity-bar toggle (or by switching to a non-issues
  // tab) when they're done.
  const handleSpawn = async (issue: GitHubIssue, providerId: string) => {
    if (activeMeshId === null) return;
    setSpawning(issue.number);
    try {
      const draft = await createIssueNode(activeMeshId, issue.number, issue.title, providerId);
      setOpenDropdown(null);
      // Dispatch stage-2 BEFORE clearing the busy state so the
      // fire-and-forget IPC is on the wire before the same-row button
      // re-enables for another click. We deliberately do NOT toggle
      // the probe — the dock stays open so the user can spawn more.
      startNodeBackground(draft.id, draft.prefill);
      setSpawning(null);
    } catch (e) {
      console.error('Failed to spawn issue agent:', e);
      setSpawning(null);
    }
  };

  // Primary "Spawn" button uses the mesh's resolved default provider —
  // explicit > per-mesh > app-wide > "anthropic" fallback is enforced
  // server-side by resolve_default_provider when we pass `provider`.
  // We mark `spawning` BEFORE awaiting getDefaultProvider so the split
  // button's `disabled` immediately blocks a second click on the same
  // issue (e.g. picking a different provider in the still-open dropdown)
  // from racing with the in-flight default-resolution IPC. If
  // `getDefaultProvider` rejects, the catch clears `spawning` so the
  // user can retry.
  const handleDefaultSpawn = async (issue: GitHubIssue) => {
    if (activeMeshId === null) return;
    setSpawning(issue.number);
    try {
      const defaultProvider = await getDefaultProvider(activeMeshId);
      const draft = await createIssueNode(activeMeshId, issue.number, issue.title, defaultProvider);
      setOpenDropdown(null);
      // Same ordering as `handleSpawn`: stage-2 IPC first, then clear
      // the busy state. Dock stays open (see handleSpawn).
      startNodeBackground(draft.id, draft.prefill);
      setSpawning(null);
    } catch (e) {
      console.error('Failed to spawn issue agent:', e);
      setSpawning(null);
    }
  };

  return (
    <div className="flex flex-col h-full">
      <div className="flex-1 overflow-y-auto p-2">
        {loading ? (
          <div className="flex flex-col items-center justify-center py-8 gap-3">
            <div className="animate-spin w-5 h-5 border border-accent-cyan border-t-transparent rounded-full" />
            <span className="text-xs text-text-muted">Loading issues...</span>
          </div>
        ) : error ? (
          <div className="flex flex-col items-center justify-center py-8">
            <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" className="text-red-400 mb-2">
              <circle cx="12" cy="12" r="10"/>
              <line x1="15" y1="9" x2="9" y2="15"/>
              <line x1="9" y1="9" x2="15" y2="15"/>
            </svg>
            <span className="text-xs text-red-400">Failed to load issues</span>
            <span className="text-[10px] text-text-muted mt-1 max-w-[280px] text-center">{error}</span>
          </div>
        ) : issues.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-8">
            <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" className="text-text-muted mb-2">
              <circle cx="12" cy="12" r="10"/>
              <line x1="12" y1="8" x2="12" y2="12"/>
              <line x1="12" y1="16" x2="12.01" y2="16"/>
            </svg>
            <span className="text-xs text-text-muted">No open issues</span>
          </div>
        ) : (
          <div className="space-y-1">
            {issues.map(issue => (
              <div
                key={issue.number}
                className="flex items-center gap-2 px-2 py-2 rounded hover:bg-bg-card transition-colors"
              >
                <div className="flex-1 min-w-0">
                  <div>
                    <span className="text-xs text-accent-cyan font-mono">#{issue.number}</span>
                    <span className="text-sm text-text-primary ml-2">{issue.title}</span>
                  </div>
                  {issue.body && (
                    <p className="text-[10px] text-text-muted mt-1 line-clamp-2">{issue.body}</p>
                  )}
                </div>

                {/* Split spawn button — primary uses default provider, ▾ opens picker */}
                <div className="relative flex shrink-0" onMouseDown={e => e.stopPropagation()}>
                  <button
                    onClick={() => handleDefaultSpawn(issue)}
                    disabled={spawning !== null}
                    className="px-2.5 py-1 text-xs font-medium rounded-l bg-accent-cyan/10 text-accent-cyan hover:bg-accent-cyan/20 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                  >
                    {spawning === issue.number ? 'Spawning...' : 'Spawn'}
                  </button>
                  <button
                    onClick={() => setOpenDropdown(openDropdown === issue.number ? null : issue.number)}
                    disabled={spawning !== null}
                    className="px-1.5 py-1 text-xs font-medium rounded-r border-l border-accent-cyan/20 bg-accent-cyan/10 text-accent-cyan hover:bg-accent-cyan/20 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                    title="Choose provider"
                  >
                    ▾
                  </button>
                  {openDropdown === issue.number && (
                    <ProviderDropdown
                      meshId={issue.number}
                      providers={providerList}
                      onSelect={(providerId) => handleSpawn(issue, providerId)}
                    />
                  )}
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
