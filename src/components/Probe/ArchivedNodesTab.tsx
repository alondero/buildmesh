/**
 * ArchivedNodesTab — the Probe Panel's Archive tab body (issue #378;
 * renamed from DiscoveredNodesTab after PR #523 set the visible label
 * to "Archive" / tooltip "Archived Nodes"). The panel shows agent
 * nodes discoverable on disk for the current mesh (Claude Code CLI
 * sessions that buildmesh has not yet adopted) and offers a one-click
 * resume.
 *
 * Thin wrapper port of the legacy `SessionBrowserModal`. The dock
 * supplies the header and close button, so this component drops the
 * modal's backdrop / header / Escape handler and renders the same
 * search input + list + split-resume button in the probe's 360px body.
 *
 * Resume flow
 * -----------
 * The primary "Resume" button uses the mesh's resolved default provider
 * (explicit > per-mesh > app-wide > "anthropic" fallback, enforced
 * server-side). The `▾` half of the split button opens a provider
 * picker filtered to providers that advertise `resumable: true` on
 * their backend-supplied `ProviderInfo` row. The backend derives the
 * flag from the resolved adapter's `supports_resume() && produces_readable_transcript()`,
 * so every Claude-compatible profile (custom ids like "DeepSeek via
 * Claude", "Kimi via Claude" included) advertises itself correctly
 * — the picker no longer hides them behind a hardcoded id allow-list
 * (#550 follow-up; see also buildmesh-gh538-unified-claude-harness).
 *
 * After a successful import we:
 *   1. `setActiveNode(node.id)` — focus the freshly-imported card.
 *   2. `selectMesh(meshId)` — keep the sidebar in sync (the node may
 *      belong to a different mesh than the one the probe is showing if
 *      the user is using the "global view" fallback).
 *   3. `spawnAgent(node.id, providerId)` — start the agent process.
 *   4. `toggleProbe()` — hide the dock so the user lands on the
 *      terminal, matching the legacy modal's `onClose()` behaviour.
 *
 * Why `activeMeshPath` from the probe context (not `activePath`)
 * ---------------------------------------------------------------
 * `activePath` resolves to the focused node's working directory (a
 * worktree subdir) when a node is active, but `discover_agent_nodes`
 * walks `.claude/projects/<project>/sessions-index.json` from the
 * *mesh root* — the same place `import_discovered_agent_node` needs
 * to attach the imported node to. The probe context hook exposes
 * `activeMeshPath` for exactly this case; we don't reach into
 * `meshesById` directly.
 */

import { useState, useMemo, useCallback } from 'react';
import {
  discoverAgentNodes,
  importDiscoveredAgentNode,
  listProviders,
  type ArchivedAgentNode,
} from '../../lib/tauri';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useProviderListInvalidation } from '../../hooks/useProviderListInvalidation';
import { GroupedProviderMenu } from '../Providers/GroupedProviderMenu';
import { mapBackendProviders } from '../../lib/groups';

function timeAgo(isoString: string): string {
  const now = Date.now();
  const then = new Date(isoString).getTime();
  const diffMs = now - then;
  const minutes = Math.floor(diffMs / 60000);
  if (minutes < 1) return 'just now';
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days}d ago`;
  return new Date(isoString).toLocaleDateString();
}

// Mirror the ProviderInfo wire shape with only the columns the picker
// needs (id/label/color for the row UI; resumable drives the filter;
// group_key / harness_id / is_proxied drive the harness-grouped render —
// issue #575). The legacy `isResumableProvider(['anthropic','minimax','kimi'])`
// allow-list was removed in #550 follow-up — custom Claude-compatible
// profiles (e.g. "DeepSeek via Claude") share the `anthropic` adapter
// and now advertise `resumable: true` themselves.
type ResumableProvider = {
  id: string;
  label: string;
  color: string;
  icon: string;
  harness_id: string;
  provider_id: string | null;
  is_proxied: boolean;
  group_key: string;
  resumable: boolean;
};

export function ArchivedNodesTab() {
  const { activeMeshId, activeMeshPath } = useProbeContext();
  // `activeMeshPath` is the mesh root, NOT the focused worktree's
  // working directory — `discover_sessions` walks `.claude/projects/...`
  // from the mesh root, and `import_discovered_session` needs to know
  // which repo to attach the imported node to. The probe context hook
  // owns this derivation.
  const getDefaultProvider = useMeshStore((s) => s.getDefaultProvider);
  const selectMesh = useMeshStore((s) => s.selectMesh);
  const fetchAgentNodes = useAgentNodeStore((s) => s.fetchAgentNodes);
  const spawnAgent = useAgentNodeStore((s) => s.spawnAgent);
  const setActiveNode = useAgentNodeStore((s) => s.setActiveNode);
  const toggleProbe = useUIStore((s) => s.toggleProbe);

  const [sessions, setSessions] = useState<ArchivedAgentNode[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [search, setSearch] = useState('');
  const [resuming, setResuming] = useState<string | null>(null);
  const [openDropdown, setOpenDropdown] = useState<string | null>(null);
  const [providerList, setProviderList] = useState<ResumableProvider[]>([]);

  // Fetch providers once at mount. Platform filtering is enforced
  // server-side; the `resumable` flag comes straight from the backend
  // (`ProviderInfo.resumable`, derived from
  // `supports_resume() && produces_readable_transcript()`) so custom
  // Claude-compatible profiles (e.g. "DeepSeek via Claude") advertise
  // themselves correctly without a frontend allow-list (#550 follow-up).
  // Wrapped in useCallback so `useProviderListInvalidation` can re-fire the
  // same fetch on the `provider-list-changed` event when the user adds or
  // removes an account in App Settings — without that, our local list goes
  // stale and the picker keeps showing accounts that no longer exist.
  // Issue #575 / ADR-0016 — preserve the Spawn Option shape so the
  // `GroupedProviderMenu` can render the harness-grouped, always-expanded
  // picker (harness header + indented Proxied children). The picker is
  // filtered to `resumable: true` rows so a non-resumable harness header
  // (e.g. Terminal) collapses to nothing rather than offering a useless
  // "click to spawn fresh" entry on a resume flow.
  // The 8-field projection lives in `mapBackendProviders`; `resumable`
  // is the one field this picker alone needs, so it's merged in here
  // rather than widening the shared `SpawnOption` shape (issue #583).
  const refreshProviderList = useCallback(() => {
    listProviders()
      .then(backendProviders => {
        const resumableById = new Map(backendProviders.map(p => [p.id, p.resumable]));
        setProviderList(
          mapBackendProviders(backendProviders).map(o => ({
            ...o,
            resumable: resumableById.get(o.id) ?? false,
          })),
        );
      })
      .catch(err => console.error('listProviders failed:', err));
  }, []);

  useAsyncEffect(() => {
    refreshProviderList();
  }, [refreshProviderList]);
  useProviderListInvalidation(refreshProviderList);

  useAsyncEffect((signal) => {
    if (activeMeshId === null || activeMeshPath === null) return;
    const load = async () => {
      try {
        const result = await discoverAgentNodes(activeMeshId, activeMeshPath);
        if (signal.aborted) return;
        setSessions(result);
      } catch (e) {
        if (signal.aborted) return;
        console.error('Failed to discover sessions:', e);
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        if (!signal.aborted) setLoading(false);
      }
    };
    setLoading(true);
    setError(null);
    load();
  }, [activeMeshId, activeMeshPath]);

  const filtered = useMemo(() => {
    const q = search.trim().toLowerCase();
    if (!q) return sessions;
    return sessions.filter(s =>
      s.first_message.toLowerCase().includes(q) ||
      (s.branch && s.branch.toLowerCase().includes(q)) ||
      (s.worktree_name && s.worktree_name.toLowerCase().includes(q))
    );
  }, [search, sessions]);

  // Close dropdown on outside click. The dropdown container carries a
  // `data-dropdown-for` attribute set to the session id, mirroring the
  // GitIssuesTab pattern — the guard prevents the mousedown that *opens*
  // the option click (or the click on the option itself) from racing
  // the document-level mousedown handler and tearing the dropdown out
  // of the DOM before the click event lands.
  // Issue #492 — shared `useClickOutside` hook.
  useClickOutside(openDropdown, () => setOpenDropdown(null));

  const handleResume = async (session: ArchivedAgentNode, providerId: string) => {
    if (activeMeshId === null || activeMeshPath === null) return;
    setResuming(session.session_id);
    setOpenDropdown(null);
    try {
      const node = await importDiscoveredAgentNode(
        activeMeshId,
        activeMeshPath,
        session.session_id,
        session.branch || 'main',
        session.worktree_name,
        providerId,
      );
      await fetchAgentNodes();
      await setActiveNode(node.id);
      // The imported node's mesh_id may differ from the one the probe
      // is showing if the user is in the "global view" fallback
      // (sidebar collapsed, focus follows the active node). Re-select
      // the mesh to keep the sidebar header in sync.
      selectMesh(activeMeshId);
      // Spawn the slow PTY-bound work first, then hide the dock. If we
      // hid first a fast user click could orphan the imported node in
      // the DB with no PTY attached.
      await spawnAgent(node.id, providerId);
      // Hide the dock so the user lands on the terminal — mirrors the
      // legacy modal's `onClose()` call after a successful resume.
      toggleProbe();
    } catch (e) {
      console.error('Failed to resume session:', e);
      setResuming(null);
    }
  };

  const handleDefaultResume = async (session: ArchivedAgentNode) => {
    if (activeMeshId === null) return;
    const defaultProvider = await getDefaultProvider(activeMeshId);
    await handleResume(session, defaultProvider);
  };

  // Filter providers to only those that support resume. The backend
  // sets `ProviderInfo.resumable` from the resolved adapter's
  // `supports_resume() && produces_readable_transcript()` so custom
  // Claude-compatible profiles (e.g. "DeepSeek via Claude") appear
  // here automatically — no frontend allow-list to maintain (#550
  // follow-up).
  const resumableProviders = providerList.filter(p => p.resumable);

  return (
    <div className="flex flex-col h-full">
      {/* Search */}
      <div className="px-3 py-2 border-b border-border-subtle">
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Filter by message, branch, or worktree…"
          className="w-full bg-bg-card border border-border-default rounded-md px-3 py-1.5 text-xs text-text-primary placeholder:text-text-muted focus:outline-none focus:border-accent-cyan"
          autoFocus
        />
      </div>

      {/* Content */}
      <div className="flex-1 overflow-y-auto p-2">
        {loading ? (
          <div className="flex flex-col items-center justify-center py-8 gap-3">
            <div className="animate-spin w-5 h-5 border border-accent-cyan border-t-transparent rounded-full" />
            <span className="text-xs text-text-muted">Scanning sessions…</span>
          </div>
        ) : error ? (
          <div className="flex flex-col items-center justify-center py-8">
            <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" className="text-red-400 mb-2">
              <circle cx="12" cy="12" r="10"/>
              <line x1="15" y1="9" x2="9" y2="15"/>
              <line x1="9" y1="9" x2="15" y2="15"/>
            </svg>
            <span className="text-xs text-red-400">Failed to discover sessions</span>
            <span className="text-[10px] text-text-muted mt-1 max-w-[280px] text-center">{error}</span>
          </div>
        ) : filtered.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-8">
            <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" className="text-text-muted mb-2">
              <circle cx="12" cy="12" r="10"/>
              <line x1="12" y1="8" x2="12" y2="12"/>
              <line x1="12" y1="16" x2="12.01" y2="16"/>
            </svg>
            <span className="text-xs text-text-muted">
              {sessions.length === 0 ? 'No previous sessions found' : 'No matches'}
            </span>
          </div>
        ) : (
          <div className="space-y-1">
            {filtered.map(session => (
              <div
                key={session.session_id}
                className="flex items-center gap-2 px-2 py-2 rounded-md hover:bg-bg-card transition-colors"
              >
                <div className="flex-1 min-w-0">
                  <div className="text-sm text-text-primary truncate">{session.first_message}</div>
                  <div className="flex items-center gap-2 mt-0.5">
                    {session.branch && (
                      <span className="text-[10px] text-accent-cyan font-mono">{session.branch}</span>
                    )}
                    {session.worktree_name && (
                      <span className="text-[10px] text-accent-purple font-mono">{session.worktree_name}</span>
                    )}
                    {session.timestamp && (
                      <span className="text-[10px] text-text-muted">{timeAgo(session.timestamp)}</span>
                    )}
                  </div>
                </div>

                {/* Split resume button */}
                <div className="relative flex shrink-0" onMouseDown={e => e.stopPropagation()}>
                  <button
                    onClick={() => handleDefaultResume(session)}
                    disabled={resuming !== null}
                    className="px-2.5 py-1 text-xs font-medium rounded-l-md bg-accent-cyan/10 text-accent-cyan hover:bg-accent-cyan/20 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                  >
                    {resuming === session.session_id ? 'Resuming…' : 'Resume'}
                  </button>
                  <button
                    onClick={() => setOpenDropdown(openDropdown === session.session_id ? null : session.session_id)}
                    disabled={resuming !== null}
                    title="Choose provider"
                    className="px-1.5 py-1 text-xs font-medium rounded-r-md border-l border-accent-cyan/20 bg-accent-cyan/10 text-accent-cyan hover:bg-accent-cyan/20 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                  >
                    ▾
                  </button>
                  {openDropdown === session.session_id && (
                    <div
                      data-dropdown-for={session.session_id}
                      className="absolute right-0 top-full mt-1 z-50 bg-bg-overlay border border-border-default rounded-md shadow-lg min-w-[200px] max-h-[320px] overflow-y-auto"
                    >
                      <GroupedProviderMenu
                        providers={resumableProviders}
                        onSelect={(providerId) => handleResume(session, providerId)}
                      />
                    </div>
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
