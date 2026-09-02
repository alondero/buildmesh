/**
 * ArchivedNodesTab — the Probe Panel's Agent History tab body (issue #378;
 * renamed from DiscoveredNodesTab after PR #523; issue #1375 renamed the
 * destination to "Agent History"). The panel shows agent
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

import { formatError } from '../../lib/errorUtils';
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
import { SpawnButtonCluster } from '../Sidebar/SpawnButtonCluster';
import {
  EmptyState,
  ErrorState,
  LoadingState,
  RefreshControl,
} from '../shared/Spinner';
import { ProbeTabBody } from './ProbeTabBody';
import { ProbeToolbar } from './ProbeToolbar';
import { mapBackendProviders, type SpawnOption } from '../../lib/groups';
import { dropdownId } from '../../lib/dropdownId';
import { formatRelativeAge } from '../../lib/time';

// The Archive tab renders a `date` fallback for items older than 30
// days (the historical "days never reset" convention). The shared
// helper floors at 24h + `days` indefinitely, so we only delegate
// the first four tiers and handle `days >= 30` locally.
function timeAgo(isoString: string): string {
  const then = new Date(isoString);
  const days = Math.floor((Date.now() - then.getTime()) / (24 * 60 * 60 * 1000));
  if (days >= 30) return then.toLocaleDateString();
  return formatRelativeAge(then, new Date());
}

// Issue #1264 — `ResumableProvider` was a hand-rolled projection that
// duplicated `SpawnOption`'s 8 fields field-for-field and added a
// single `resumable: boolean`. The two shapes drifted silently the day
// someone added a new column to `SpawnOption` and forgot this one.
// Express the relationship directly: a resumable provider IS a
// `SpawnOption` with the resume flag tacked on.
type ResumableProvider = SpawnOption & { resumable: boolean };

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
  // Bump to force the load effect to re-run on a manual Refresh click
  // (issue #813 — Archive previously had no manual-refresh affordance).
  // The mirror of the existing GitIssuesTab / GitPullRequestsTab
  // `reloadKey` pattern — bumping aborts the previous effect's signal
  // (useAsyncEffect) and refetches.
  // Bump to refetch on manual Refresh (issue #813 added the Refresh
  // affordance to this tab).
  const [reloadKey, setReloadKey] = useState(0);
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
        setError(formatError(e));
      } finally {
        if (!signal.aborted) setLoading(false);
      }
    };
    setLoading(true);
    setError(null);
    load();
  }, [activeMeshId, activeMeshPath, reloadKey]);

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
      {/* Search + manual Refresh share one row so the prior
          "search full-width / refresh elsewhere" jump on tab
          switch doesn't reappear. */}
      <ProbeToolbar
        trailing={
          <RefreshControl
            onRefresh={() => setReloadKey((k) => k + 1)}
            isRefreshing={loading && sessions.length > 0}
            ariaLabel="Refresh archived sessions"
          />
        }
      >
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Filter by message, branch, or worktree…"
          className="flex-1 min-w-0 bg-bg-card border border-border-default rounded-md px-3 py-1.5 text-xs text-text-primary placeholder:text-text-muted focus:outline-none focus:border-accent-cyan"
          autoFocus
        />
      </ProbeToolbar>

      {/* Content */}
      <ProbeTabBody padding="p-3">
        {loading && sessions.length === 0 ? (
          <LoadingState label="Scanning sessions…" />
        ) : error ? (
          <ErrorState title="Failed to discover sessions" detail={error} />
        ) : filtered.length === 0 ? (
          <EmptyState
            label={sessions.length === 0 ? 'No previous sessions found' : 'No matches'}
          />
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
                      <span className="text-2xs text-accent-cyan font-mono">{session.branch}</span>
                    )}
                    {session.worktree_name && (
                      <span className="text-2xs text-accent-violet font-mono">{session.worktree_name}</span>
                    )}
                    {session.timestamp && (
                      <span className="text-2xs text-text-secondary tabular-nums">{timeAgo(session.timestamp)}</span>
                    )}
                  </div>
                </div>

                {/* Split Resume Menu cluster — ADR-0016 §2 / issue #813.
                    Primary label "Resume" + session-id key land the
                    archived-resume flow on the same `<SpawnButtonCluster>`
                    as Issues/PRs/Sidebar. `getDefaultProvider` is
                    undefined when `activeMeshId === null` so the cluster
                    skips its tooltip fetch (the click path still works
                    via `handleDefaultResume`'s own null guard). */}
                <div
                  className="shrink-0"
                  onMouseDown={e => e.stopPropagation()}
                >
                  <SpawnButtonCluster
                    providers={resumableProviders}
                    // Issue #1264 — surface prefix keeps this menu's
                    // `data-dropdown-for` from colliding with a
                    // node- or mesh-keyed menu on the same id
                    // (session ids are stringly-typed but the
                    // collision risk is the same shape).
                    dropdownKey={dropdownId('session', session.session_id)}
                    isOpen={openDropdown === session.session_id}
                    primaryLabel="Resume"
                    busyLabel="Resuming…"
                    primaryAriaLabel="Resume session"
                    onToggleDropdown={() =>
                      setOpenDropdown(openDropdown === session.session_id ? null : session.session_id)
                    }
                    onSpawnDefault={() => handleDefaultResume(session)}
                    onSelectProvider={(providerId) => handleResume(session, providerId)}
                    disabled={resuming !== null}
                    isSpawning={resuming === session.session_id}
                    // The store's `getDefaultProvider` requires a
                    // non-null mesh id; the cluster calls this from
                    // hover/focus to populate its tooltip. Drop the
                    // tooltip affordance when the probe hasn't focused
                    // a mesh yet (the Resume buttons stay clickable —
                    // `handleDefaultResume` has its own null guard).
                    getDefaultProvider={activeMeshId !== null ? () => getDefaultProvider(activeMeshId) : undefined}
                  />
                </div>
              </div>
            ))}
          </div>
        )}
      </ProbeTabBody>
    </div>
  );
}
