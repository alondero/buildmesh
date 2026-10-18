/**
 * ArchivedNodesTab — the Probe Panel's Agent History tab body (issue #378;
 * renamed from DiscoveredNodesTab after PR #523; issue #1375 renamed the
 * destination to "Agent History"). The panel shows agent
 * nodes discoverable on disk for the current mesh (Claude Code CLI
 * sessions that buildmesh has not yet adopted) and offers a one-click
 * resume.
 *
 * The list is also the durable half of the archive (issue #1065): an
 * `agent_nodes` row Buildmesh archived whose transcript is gone or was
 * never written is listed too, flagged `resumable: false`, because its
 * worktree and branch still exist even though there is nothing to resume.
 *
 * Thin wrapper port of the legacy `SessionBrowserModal`. The dock
 * supplies the header and close button, so this component drops the
 * modal's backdrop / header / Escape handler and renders the same
 * search input + list + split-resume button in the probe's 360px body.
 *
 * Resume flow
 * -----------
 * The primary "Resume" button resumes through the harness that created the
 * conversation (issue #2332). A resumable discovery entry carries the harness
 * that owns it; a durable row carries the executor it was recorded with. The
 * default button is that harness's own Spawn Option rather than the mesh's
 * resolved default provider — resuming a Cursor or Antigravity conversation
 * through Claude Code lands in an empty session. The `▾` half of the split
 * button opens a provider picker filtered to providers that advertise
 * `resumable: true` on their backend-supplied `ProviderInfo` row, so picking
 * another executor stays available as an explicit choice. The
 * backend derives that flag from the resolved adapter's `supports_resume()
 * && produces_readable_transcript()`, so every Claude-compatible profile
 * (custom ids like "DeepSeek via Claude", "Kimi via Claude" included)
 * advertises itself correctly — the picker no longer hides them behind a
 * hardcoded id allow-list (#550 follow-up; see also
 * buildmesh-gh538-unified-claude-harness).
 *
 * An entry whose harness is unknown, or whose harness has no option here
 * (not installed), shows "Resume unavailable" with the reason in its tooltip:
 * there is no honest executor to offer, and the backend would refuse the
 * import anyway (#2332).
 *
 * After import, navigation focuses the card and enters its mesh. The
 * agent process starts before the dock closes so the terminal is ready.
 *
 * Why `activeMeshPath` from the probe context (not `activePath`)
 * ---------------------------------------------------------------
 * `activePath` resolves to the focused node's working directory (a
 * worktree subdir) when a node is active, but `discover_agent_nodes`
 * walks `.claude/projects/<project>/sessions-index.json` from the
 * mesh root. Import attaches the node to the path stored on the mesh
 * row, so the path passed with the import is not that directory.
 * The probe context hook exposes `activeMeshPath` for the discovery
 * walk; we don't reach into `meshesById` directly.
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
import { enterMesh } from '../../lib/navigation';
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
import { formatListTimestamp } from '../../lib/time';
import { adapterIdFromProvider, archiveHarnessLabel, resumeOptionForHarness } from '../../lib/archiveProvenance';

// Issue #2461 — `timeAgo` used to live here: the relative age this tab
// renders, with a local `days >= 30` cut to an absolute date (the
// historical "days never reset" convention). The Agent History cards need
// the identical label, so it moved to `lib/time.ts` as
// `formatListTimestamp` rather than becoming a second copy.

// Issue #1264 — `ResumableProvider` was a hand-rolled projection that
// duplicated `SpawnOption`'s 8 fields field-for-field and added a
// single `resumable: boolean`. The two shapes drifted silently the day
// someone added a new column to `SpawnOption` and forgot this one.
// Express the relationship directly: a resumable provider IS a
// `SpawnOption` with the resume flag tacked on.
type ResumableProvider = SpawnOption & { resumable: boolean };

// Issue #2332 — why a row offers no Resume. Three dead ends with three
// different causes, and collapsing them into one message sends the user
// after the wrong thing: a missing transcript, a harness this build cannot
// name, or a harness that is simply not installed. One function so the row
// tooltip and the resume path's error cannot drift apart. The sentences are
// the *reason*; the control next to them already says "Resume unavailable".
function resumeUnavailableReason(session: ArchivedAgentNode): string {
  if (!session.resumable) {
    return 'This archived node has no transcript on disk, so there is nothing to resume.';
  }
  const label = archiveHarnessLabel(session.provider);
  if (!label) {
    return 'Buildmesh does not know which agent harness created this conversation.';
  }
  return `${label} is not installed.`;
}

export function ArchivedNodesTab({ meshId, meshPath }: { meshId?: number; meshPath?: string } = {}) {
  const context = useProbeContext();
  const activeMeshId = meshId ?? context.activeMeshId;
  const activeMeshPath = meshPath ?? context.activeMeshPath;
  // `activeMeshPath` is the mesh root, NOT the focused worktree's
  // working directory — `discover_sessions` walks `.claude/projects/...`
  // from the mesh root, and `import_discovered_session` needs to know
  // which repo to attach the imported node to. The probe context hook
  // owns this derivation.
  const getDefaultProvider = useMeshStore((s) => s.getDefaultProvider);
  const fetchAgentNodes = useAgentNodeStore((s) => s.fetchAgentNodes);
  const adoptAgentNode = useAgentNodeStore((s) => s.adoptAgentNode);
  const spawnAgent = useAgentNodeStore((s) => s.spawnAgent);
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
  // `data-dropdown-for` attribute set to the prefixed session id, mirroring
  // the GitIssuesTab pattern — the guard prevents the mousedown that *opens*
  // the option click (or the click on the option itself) from racing
  // the document-level mousedown handler and tearing the dropdown out
  // of the DOM before the click event lands.
  // Issue #492 — shared `useClickOutside` hook. The key must be the same
  // pre-prefixed value the cluster tags its root with
  // (`dropdownId('session', ...)`) — the raw session id matches nothing,
  // which today is only masked by the row's mousedown stopPropagation
  // wrapper around the cluster.
  useClickOutside(openDropdown !== null ? dropdownId('session', openDropdown) : null, () => setOpenDropdown(null));

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
      // Import already returned the durable row; a failed refresh must not
      // prevent the explicit resume from acting on that successful result.
      adoptAgentNode(node);
      await fetchAgentNodes();
      enterMesh(node.mesh_id, { nodeId: node.id });
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

  // Issue #2332 — Resume runs through the harness that owns the conversation.
  // The dropdown stays an explicit executor choice. Nothing here falls back to
  // the mesh default provider when it points at a *different* harness: a Cursor
  // conversation launched through Claude Code resumes nothing, so an
  // unresolvable harness has to surface instead of guess.
  const ownHarnessOption = (session: ArchivedAgentNode) =>
    resumeOptionForHarness(resumableProviders, session.provider);

  /**
   * The executor the primary Resume button uses, and the one its tooltip
   * names.
   *
   * A mesh default that already belongs to this conversation's harness keeps
   * its Model Provider choice — a Proxied Provider row like `claude:minimax`
   * is how that user runs Claude Code, and dropping it would silently switch
   * them to the self-auth account. Only a default pointing at a *different*
   * harness is replaced by the conversation's own one, which is the case the
   * issue filed.
   *
   * Takes the conversation's own option as an argument rather than looking it
   * up again: the row already resolved it to decide whether to render the
   * button at all, and passing it makes "there is an executor" total instead
   * of a `null` the caller has to re-check.
   */
  const defaultResumeId = async (
    session: ArchivedAgentNode,
    own: ResumableProvider,
  ): Promise<string> => {
    if (activeMeshId === null) return own.id;
    const meshDefault = await getDefaultProvider(activeMeshId);
    const sessionHarness = adapterIdFromProvider(session.provider ?? '');
    return adapterIdFromProvider(meshDefault) === sessionHarness ? meshDefault : own.id;
  };

  const handleDefaultResume = async (
    session: ArchivedAgentNode,
    own: ResumableProvider,
  ) => {
    await handleResume(session, await defaultResumeId(session, own));
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
            {filtered.map(session => {
              // Issue #2332 — resolved once per row; `ownOption` is also what
              // decides whether the Resume control is rendered at all, so the
              // two cannot disagree about whether this entry is resumable.
              const harnessLabel = archiveHarnessLabel(session.provider);
              const ownOption = ownHarnessOption(session);
              return (
                <div
                  key={session.session_id}
                  className="flex items-center gap-2 px-2 py-2 rounded-md hover:bg-bg-card transition-colors"
                >
                  <div className="flex-1 min-w-0">
                    <div className="text-sm text-text-primary truncate">{session.first_message}</div>
                    <div className="flex items-center gap-2 mt-0.5">
                      {session.branch && (
                        <span className="text-2xs text-text-secondary font-mono">{session.branch}</span>
                      )}
                      {session.worktree_name && (
                        <span className="text-2xs text-accent-violet font-mono">{session.worktree_name}</span>
                      )}
                      {/* Issue #2332 — which harness owns the conversation,
                          stated before the user opens the picker. */}
                      {harnessLabel && (
                        <span
                          className="text-2xs text-text-secondary"
                          data-testid={`archive-harness-${session.session_id}`}
                        >
                          {harnessLabel}
                        </span>
                      )}
                      {session.timestamp && (
                        <span className="text-2xs text-text-secondary tabular-nums">{formatListTimestamp(session.timestamp)}</span>
                      )}
                    </div>
                  </div>

                  {/* Split Resume Menu cluster — ADR-0016 §2 / issue #813.
                      Primary label "Resume" + session-id key land the
                      archived-resume flow on the same `<SpawnButtonCluster>`
                      as Issues/PRs/Sidebar.

                      Issue #1065 — a durable archived row with no discoverable
                      transcript (`resumable: false`) has no resume target at
                      all: importing it would create a node whose session id
                      points at a conversation the harness cannot find. Show
                      the reason instead of a button that cannot work.

                      Issue #2332 — an entry with a transcript but no known
                      harness is the same shape of dead end, for the same
                      reason: the backend refuses to guess an executor, so the
                      button is withheld rather than offered and refused. */}
                  <div
                    className="shrink-0"
                    onMouseDown={e => e.stopPropagation()}
                  >
                    {session.resumable && ownOption ? (
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
                        configurationsEnabled={false}
                        busyLabel="Resuming…"
                        primaryAriaLabel="Resume session"
                        onToggleDropdown={() =>
                          setOpenDropdown(openDropdown === session.session_id ? null : session.session_id)
                        }
                        onSpawnDefault={() => handleDefaultResume(session, ownOption)}
                        onSelectProvider={(providerId) => handleResume(session, providerId)}
                        disabled={resuming !== null}
                        isSpawning={resuming === session.session_id}
                        // Issue #2332 — the tooltip has to name the executor
                        // this button will actually use, which is the
                        // conversation's own harness (or the mesh default
                        // when that default already belongs to that harness).
                        getDefaultProvider={async () =>
                          await defaultResumeId(session, ownOption)
                        }
                      />
                    ) : (
                      <span
                        className="text-2xs text-text-secondary"
                        title={resumeUnavailableReason(session)}
                      >
                        Resume unavailable
                      </span>
                    )}
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </ProbeTabBody>
    </div>
  );
}
