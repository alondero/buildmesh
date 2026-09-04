/**
 * GitPullRequestsTab — the Probe Panel's Pull Requests tab body.
 *
 * Sibling of `GitIssuesTab`: same dock-supplied header / close button, same
 * `useProbeContext` mesh scoping, same loading / error / empty skeleton. Lists
 * the active mesh's pull requests (open by default, with a toggle to closed)
 * and lets the user squash-merge a mergeable open PR straight from the panel.
 *
 * Mergeability is a single cohesive query (issue #1529)
 * -----------------------------------------------------
 * `getRepoPulls` returns list fields plus `mergeable`/`mergeable_state`
 * inline via the backend's GraphQL PR-summaries connection — one HTTP
 * request per page, not one per PR. The panel never orchestrates per-row
 * enrichment calls. A draft PR is flagged without consulting mergeability.
 *
 * Re-poll on `mergeable: null` (issue #419, preserved through #1529)
 * ------------------------------------------------------------------
 * GitHub computes mergeability asynchronously, so a fresh/updated PR can
 * arrive as `mergeable: null` (`UNKNOWN`). The load effect schedules ONE
 * shared timer that refetches the whole list (still a single IPC /
 * GraphQL page per attempt — the O(pages) contract holds on the retry
 * path too): first retry after 1.5s, then 3s, 4.5s, giving up after 3
 * re-polls (~9s total, mirroring the old per-PR budget). Past that the
 * row renders "Unknown" with an inline retry instead of sitting on
 * "Checking…" forever; the retry re-arms the budget and refetches.
 * Whole-list transport failures (rate limit, network) surface as the
 * panel-level error with manual retry. Stale list responses are dropped via
 * the `useAsyncEffect` abort signal when the mesh/filter changes.
 *
 * Merge is squash + delete branch (the existing `merge_pr`), gated behind an
 * inline confirm because it's an irreversible outward action. On success the
 * list refetches so the merged PR drops out of the open view.
 *
 * Read-oriented companion (issue #421): each row has a "View changes" button
 * that opens the PR's diff in the Center Workspace Diff Overlay
 * (`openDiff({ source: 'pr', prNumber, filePath: '', … })`). The overlay
 * fetches via `getPrFiles` (GitHub's `/pulls/{n}/files`) and renders a
 * generous file list → click a file to see its patch. This complements the
 * merge action without overlapping it: merge writes, view reads.
 *
 * Spawn companion (issue #420): open PRs get a split spawn button (default
 * provider + ▾ picker) that mirrors `GitIssuesTab`'s issue-spawn flow. The
 * backend `create_pr_node` creates a `pending` node with the PR's head ref
 * stored in `branch` (and `source_pr` set) and starts the intent-driven
 * launch in the background — which fetches `origin/<head_ref>` and cuts
 * the worktree from it so the agent lands on the same commits the PR is
 * built from (worktree adoption, #36 follow-up). The dock stays open
 * after a successful spawn (matches the issue-tab behaviour, see memory
 * buildmesh-spawn-from-probe-keeps-dock-open).
 *
 * Read-the-body companion (issue #461): mirror of PR #459's
 * `GitIssuesTab` pattern. Body is clamped to 2 lines; clicking it flips
 * to a scrollable container (`max-h-48`, `whitespace-pre-wrap`,
 * `break-words`). Title is an `<a target="_blank" rel="noopener
 * noreferrer">` to `pr.url` with a `↗` discoverability hint, and the
 * empty-URL guard (defensive — `PullRequest.html_url` has no
 * `#[serde(default)]`, unlike the issue struct) renders the title as
 * a `<span>` and omits the icon. The expanded Set resets on every
 * load (mesh OR open/closed filter — PR numbers are not stable across
 * either boundary).
 */

import { formatError } from '../../lib/errorUtils';
import { useState, useCallback, useRef } from 'react';
import {
  getRepoPulls,
  mergePr,
  createPrNode,
  listProviders,
  type GitHubPullRequest,
} from '../../lib/tauri';
import { useMeshStore } from '../../stores/meshStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useProbeContext } from '../../hooks/useProbeContext';
import { refreshOpenPrByPath } from '../../hooks/useOpenPr';
import { getNodeGitPath } from '../../lib/paths';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { useToggleSet } from '../../hooks/useToggleSet';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useProviderListInvalidation } from '../../hooks/useProviderListInvalidation';
import { useMeshGitHubUrl } from '../../hooks/useMeshGitHubUrl';
import { useUIStore } from '../../stores/uiStore';
import { mapBackendProviders, type SpawnOption } from '../../lib/groups';
import { dropdownId } from '../../lib/dropdownId';
import { SpawnButtonCluster } from '../Sidebar/SpawnButtonCluster';
import { ProbeRow } from './ProbeRow';
import { ProbeTabBody } from './ProbeTabBody';
import { ProbeToolbar } from './ProbeToolbar';
import { SafeLink } from '../shared/SafeLink';
import {
  EmptyState,
  ErrorState,
  LoadingState,
  RefreshControl,
} from '../shared/Spinner';

type StateFilter = 'open' | 'closed';

/** Derived merge readiness for one open PR (issue #1529 — inline fields). */
type MergeStatus =
  | { kind: 'checking' }
  | { kind: 'mergeable' }
  | { kind: 'blocked'; label: string }
  | { kind: 'unknown' };

// Flag wording for a non-mergeable PR, keyed by GitHub's `mergeable_state`
// (lowercase REST vocabulary mapped from GraphQL `mergeStateStatus`).
// Module-level so it isn't rebuilt on every `deriveMergeStatus` call.
const BLOCKED_WORDING: Record<string, string> = {
  dirty: 'Conflicts',
  blocked: 'Blocked',
  behind: 'Behind',
  unstable: 'Unstable',
};

// Re-poll budget for `mergeable: null` rows (issue #419 via #1529): at most
// this many list refetches after the initial load, spaced BASE_POLL_DELAY_MS
// * attempt (1.5s, 3s, 4.5s — ~9s total, mirroring the old per-PR budget).
// Each attempt is one list IPC / GraphQL page, so the retry path stays
// O(pages). Module-level so the load effect and the tests share them.
const MAX_POLL_ATTEMPTS = 3;
const BASE_POLL_DELAY_MS = 1500;

// --- Icon components ------------------------------------------------------
// Tiny inline SVG icons used by the row's action buttons. The previous
// text labels ("Merge", "View changes") ate ~50-80px each in the 360px
// probe dock; icon-only buttons reclaim the space and read as clearly
// (or more clearly) at a glance. The pattern matches the existing
// error/empty-state SVGs in this file: 24×24 viewBox, no fill, 1.5px
// stroke, currentColor — so the icon adopts whatever text colour the
// button applies (cyan for merge, green for confirm, muted for cancel).
//
// All four take a `className` so the caller can size the icon (e.g. `w-3.5
// h-3.5`). Width/height attributes are intentionally left at the SVG
// defaults (24) and the visible size is controlled by the wrapper classes.

function GitMergeIcon({ className }: { className?: string }) {
  // Lucide's git-merge: two end nodes joined by a trunk + a side branch.
  // Universally recognised as "merge branches / merge PR".
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.75"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <circle cx="18" cy="18" r="3" />
      <circle cx="6" cy="6" r="3" />
      <path d="M6 21V9a9 9 0 0 0 9 9" />
    </svg>
  );
}

function FileTextIcon({ className }: { className?: string }) {
  // Lucide's file-text: a file outline with three content lines. Reads
  // as "view file" / "read content" — appropriate for the "open this
  // PR's diff in the center overlay" action.
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.75"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
      <polyline points="14 2 14 8 20 8" />
      <line x1="8" y1="13" x2="16" y2="13" />
      <line x1="8" y1="17" x2="13" y2="17" />
      <line x1="8" y1="9" x2="10" y2="9" />
    </svg>
  );
}

function CheckIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.25"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <polyline points="20 6 9 17 4 12" />
    </svg>
  );
}

function XIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2.25"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <line x1="18" y1="6" x2="6" y2="18" />
      <line x1="6" y1="6" x2="18" y2="18" />
    </svg>
  );
}

/**
 * Turn a PR with inline mergeability (issue #1529) into a display status.
 * `draft` short-circuits to blocked. `mergeable: null` (GitHub `UNKNOWN`,
 * still computing) is "checking" while the bounded re-poll below still has
 * budget, and "unknown" (with an inline retry) once it is exhausted — both
 * visually distinct from conflict and from the panel-level transport error.
 *
 * `mergeable` answers only "does the tree merge cleanly". When it is true
 * but `mergeStateStatus` reports a policy gate (`blocked`: required checks
 * / approvals missing, `behind`: branch out of date, `dirty`: conflicts
 * reported after all), the merge API would reject the squash — so the row
 * shows the gate instead of a Merge button that cannot succeed.
 */
function deriveMergeStatus(pr: GitHubPullRequest, pollExhausted: boolean): MergeStatus {
  if (pr.draft) return { kind: 'blocked', label: 'Draft' };
  // `??` covers old cached payloads missing the #1529 fields (the wire
  // defaults them, but a stale renderer could still see `undefined`).
  const state = pr.mergeable_state ?? 'unknown';
  const mergeable = pr.mergeable ?? null;
  if (mergeable === null) return pollExhausted ? { kind: 'unknown' } : { kind: 'checking' };
  if (mergeable === false) {
    // mergeable === false — word the flag from GitHub's mergeable_state.
    return { kind: 'blocked', label: BLOCKED_WORDING[state] ?? 'Conflicts' };
  }
  // mergeable === true with a policy gate still shows the gate.
  if (state === 'blocked' || state === 'behind' || state === 'dirty') {
    return { kind: 'blocked', label: BLOCKED_WORDING[state] ?? 'Conflicts' };
  }
  return { kind: 'mergeable' };
}

export function GitPullRequestsTab() {
  const { activeMeshId, activeMeshPath } = useProbeContext();
  const openDiff = useUIStore((s) => s.openDiff);
  // `getDefaultProvider` is mesh-scoped — same pattern as `GitIssuesTab`.
  const getDefaultProvider = useMeshStore((s) => s.getDefaultProvider);

  const [prs, setPrs] = useState<GitHubPullRequest[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [stateFilter, setStateFilter] = useState<StateFilter>('open');
  // Which PR is awaiting an inline merge confirm, and which is mid-merge.
  const [confirming, setConfirming] = useState<number | null>(null);
  const [merging, setMerging] = useState<number | null>(null);
  // Per-row merge failure message (transient `gh`/network hiccup, etc.).
  const [mergeError, setMergeError] = useState<Record<number, string>>({});
  // Spawn state (issue #420) — mirrors the issue-tab pattern: a per-PR
  // "spawning" flag for the button, an "open dropdown" for the ▾ picker, and
  // a per-PR spawn error so a rejected `create_pr_node` surfaces inline
  // rather than vanishing into the console.
  const [spawning, setSpawning] = useState<number | null>(null);
  const [openDropdown, setOpenDropdown] = useState<number | null>(null);
  const [spawnError, setSpawnError] = useState<Record<number, string>>({});
  const [providerList, setProviderList] = useState<SpawnOption[]>([]);
  // Per-row expand state (issue #461) — Set keyed by PR number so two
  // long PRs can stay expanded side-by-side. Reset on every load
  // below (mesh + open/closed filter both change the visible set of
  // PR numbers, so the prior Set would either no-op or re-open a
  // different row). `useToggleSet` (issue #463) bundles the Set state
  // + toggle closure + clear reset into one hook.
  const expanded = useToggleSet<number>();
  // Bump to force the load effect to re-run — the previous run's signal
  // is aborted by useAsyncEffect's cleanup, so any in-flight getRepoPulls
  // drops its setState instead of clobbering the new run's result. Used
  // by `handleMerge` to refetch after a successful squash-merge (issue #349).
  // Bump to refetch on manual Refresh or after merge (issue #349
  // refetch; #813 surfaced Refresh to the user). `useAsyncEffect`
  // aborts the previous effect's signal on dep change.
  const [reloadKey, setReloadKey] = useState(0);
  // Bounded re-poll budget for `mergeable: null` rows (issue #419, kept
  // through #1529). `pollAttempts` survives reloadKey bumps (the re-poll's
  // own refetches must consume budget, not reset it) and resets only when
  // the scope changes (mesh/filter, tracked below) or the list resolves
  // with no nulls left. `pollExhausted` flips the null rows from
  // "Checking…" to the "Unknown" retry state the status derives from.
  // `softReload` marks reloads that must preserve row state (background
  // timer ticks and row-level Unknown retries) so the load effect can skip
  // the row-state clears below — those must never collapse an expanded row
  // or cancel a pending merge confirm under the user's nose.
  // `softReload` does the same for a row's own Unknown-retry button. Only
  // a scope change or an explicit toolbar Refresh / merge refetch clears.
  const pollAttempts = useRef(0);
  const softReload = useRef(false);
  const prevScope = useRef<{ meshId: number | null; filter: StateFilter } | null>(null);
  const [pollExhausted, setPollExhausted] = useState(false);
  // "View on GitHub" header button — resolves the active mesh's
  // `origin` to a `https://github.com/{owner}/{repo}/pulls` URL.
  // Mirror of GitIssuesTab's hook call; both tabs share the same
  // per-mesh cache so the IPC is deduped when the user toggles
  // between the two probes on the same mesh.
  const { url: githubUrl } = useMeshGitHubUrl(activeMeshId, activeMeshPath);
  const pullsListUrl = githubUrl ? `${githubUrl}/pulls` : '';

  // Re-arm the re-poll budget and refetch. The toolbar Refresh is a hard
  // reload (row state clears in the effect); a row's own Unknown-retry is
  // soft (the user may be reading another expanded row).
  const retryListLoad = useCallback(() => {
    pollAttempts.current = 0;
    setPollExhausted(false);
    setReloadKey((k) => k + 1);
  }, []);
  const retryUnknownRow = useCallback(() => {
    pollAttempts.current = 0;
    setPollExhausted(false);
    softReload.current = true;
    setReloadKey((k) => k + 1);
  }, []);

  // Fresh scope, fresh budget: tracked inline in the load effect (no extra
  // effect lifecycle for two synchronous resets).
  useAsyncEffect((signal) => {
    if (activeMeshId === null) return;
    // Scope change (mesh/filter) is a full reset: fresh budget, and the
    // row state below is cleared (PR numbers don't carry across scopes).
    // A background poll tick (or explicit refresh/merge refetch) keeps the
    // running budget and — for poll ticks only — the row state: collapsing
    // an expanded row or cancelling a pending merge confirm under the
    // user's nose on a timer tick would be a UX glitch.
    const scopeChanged =
      prevScope.current === null ||
      prevScope.current.meshId !== activeMeshId ||
      prevScope.current.filter !== stateFilter;
    prevScope.current = { meshId: activeMeshId, filter: stateFilter };
    if (scopeChanged) {
      pollAttempts.current = 0;
      setPollExhausted(false);
    }
    const isSoftReload = softReload.current;
    softReload.current = false;
    setLoading(true);
    setError(null);
    if (!isSoftReload) {
      setConfirming(null);
      setMergeError({});
      // PR numbers don't carry across mesh/filter changes (issue #461).
      expanded.clear();
    }
    // One shared re-poll timer per load (issue #419 via #1529: a single
    // list refetch, not one timer per stuck PR). Owned by this effect run;
    // cleanup clears it so unmount/mesh/filter/reload never leaks a retry.
    let pollTimer: ReturnType<typeof setTimeout> | null = null;
    (async () => {
      try {
        const result = await getRepoPulls(activeMeshId, stateFilter);
        // The mesh / filter could have changed mid-flight — drop a stale result.
        if (signal.aborted) return;
        setPrs(result);
        // Closed PRs show no merge control, so unknown mergeability there
        // needs no resolution. Drafts render "Draft" without consulting it.
        const stillNull =
          stateFilter === 'open' &&
          result.some((pr) => !pr.draft && (pr.mergeable ?? null) === null);
        if (!stillNull) {
          pollAttempts.current = 0;
          setPollExhausted(false);
          return;
        }
        if (pollAttempts.current >= MAX_POLL_ATTEMPTS) {
          setPollExhausted(true);
          return;
        }
        const delay = BASE_POLL_DELAY_MS * (pollAttempts.current + 1);
        pollTimer = setTimeout(() => {
          pollTimer = null;
          if (signal.aborted) return;
          pollAttempts.current += 1;
          softReload.current = true;
          setReloadKey((k) => k + 1);
        }, delay);
      } catch (e) {
        if (signal.aborted) return;
        console.error('Failed to load pull requests:', e);
        setError(formatError(e));
      } finally {
        if (!signal.aborted) setLoading(false);
      }
    })();

    return () => {
      if (pollTimer !== null) clearTimeout(pollTimer);
    };
  }, [activeMeshId, stateFilter, reloadKey]);

  // Fetch the provider list once at mount (issue #420, mirrors
  // `GitIssuesTab`). Platform filtering is enforced server-side via
  // `AgentProvider::available_on()`. The list can change during a session
  // when the user adds or removes a custom provider in App Settings — the
  // hook below re-fires this fetch on the `provider-list-changed` event so
  // the PR-spawn picker drops stale accounts without an app restart.
  // Issue #575 / ADR-0016 — preserve the Spawn Option shape so the
  // `ProviderDropdown` can render the harness-grouped, always-expanded
  // menu (harness header + indented Proxied children). The 8-field
  // projection lives in `mapBackendProviders` (issue #583 cleanup).
  const refreshProviderList = useCallback(() => {
    listProviders()
      .then((backendProviders) => setProviderList(mapBackendProviders(backendProviders)))
      .catch((err) => console.error('listProviders failed:', err));
  }, []);

  useAsyncEffect(() => { refreshProviderList(); }, [refreshProviderList]);
  useProviderListInvalidation(refreshProviderList);

  // Close the provider dropdown when clicking outside it. The dropdown
  // container carries a `data-dropdown-for` attribute set to the PR number,
  // matching the issue-tab pattern (memory:
  // feedback-probe-tab-test-and-jsdoc-gotchas — mousedown vs click race).
  // Issue #492 — shared `useClickOutside` hook; the scoped selector lives
  // in the hook so a future caller cannot reintroduce the loose-selector
  // drift that #492 fixed in Sidebar.
  useClickOutside(openDropdown, () => setOpenDropdown(null));

  // One backend-owned acceptance call. The node-created event makes the row
  // visible immediately; completion/failure events arrive when the intent
  // spawner has registered (or rejected) the PTY.
  const handleSpawn = async (pr: GitHubPullRequest, providerId: string) => {
    if (activeMeshId === null) return;
    setSpawning(pr.number);
    setSpawnError((prev) => {
      const next = { ...prev };
      delete next[pr.number];
      return next;
    });
    try {
      await createPrNode(
        activeMeshId,
        pr.number,
        pr.title,
        pr.head_ref,
        // Issue #444 — pass the PR's head SHA so the backend can pin the
        // worktree to that exact commit and emit a `pr_sha_drift`
        // `mesh-sync-warning` if the PR was force-pushed / rebased
        // between click-time and spawn-time. An empty `pr.head_sha`
        // (partial GitHub response, fork payload) is passed through
        // unchanged; the backend treats empty as "skip the drift check"
        // — same fail-open semantics as `pr_head_unfetchable`.
        pr.head_sha,
        providerId,
        pr.head_repo_owner,
        pr.head_repo_clone_url,
      );
      setOpenDropdown(null);
      setSpawning(null);
    } catch (e) {
      console.error('Failed to spawn PR agent:', e);
      setSpawnError((prev) => ({
        ...prev,
        [pr.number]: formatError(e),
      }));
      setSpawning(null);
    }
  };

  // Primary "Spawn" button uses the mesh's resolved default provider —
  // explicit > per-mesh > app-wide > "anthropic" fallback is enforced
  // server-side by `resolve_default_provider`. We mark `spawning` BEFORE
  // awaiting `getDefaultProvider` so the split button's `disabled`
  // immediately blocks a second click on the same PR (e.g. picking a
  // different provider in the still-open dropdown) from racing with the
  // in-flight default-resolution IPC. If `getDefaultProvider` rejects, the
  // catch clears `spawning` so the user can retry.
  const handleDefaultSpawn = async (pr: GitHubPullRequest) => {
    if (activeMeshId === null) return;
    setSpawning(pr.number);
    setSpawnError((prev) => {
      const next = { ...prev };
      delete next[pr.number];
      return next;
    });
    try {
      const defaultProvider = await getDefaultProvider(activeMeshId);
      await createPrNode(
        activeMeshId,
        pr.number,
        pr.title,
        pr.head_ref,
        pr.head_sha,
        defaultProvider,
        pr.head_repo_owner,
        pr.head_repo_clone_url,
      );
      setOpenDropdown(null);
      setSpawning(null);
    } catch (e) {
      console.error('Failed to spawn PR agent:', e);
      setSpawnError((prev) => ({
        ...prev,
        [pr.number]: formatError(e),
      }));
      setSpawning(null);
    }
  };

  const handleMerge = async (pr: GitHubPullRequest) => {
    setMerging(pr.number);
    setConfirming(null);
    setMergeError((prev) => {
      const next = { ...prev };
      delete next[pr.number];
      return next;
    });
    try {
      await mergePr(pr.url);
      // Issue #780 — force-invalidate the Open PR cache for every
      // agent node whose branch matches the merged PR's head ref, so
      // the chip in GridNodeHeader flips to "no open PR" immediately
      // instead of lagging up to 60s behind the merge while the
      // bus-driven freshness window suppresses the next GIT_CHANGED
      // refetch. We match by `branch` (the agent node's working
      // branch) rather than refreshing the whole mesh so the
      // invalidation is scoped to chips that actually change.
      // `getNodeGitPath` mirrors the path the chip's `useOpenPr`
      // subscribed to — worktree subdir for a Worktree Node, mesh
      // root for a Root Node.
      if (pr.head_ref && activeMeshId !== null) {
        // Snapshot at click-time (not a reactive subscription) — we
        // only need the current set of nodes to find matching branches
        // for this one merge, and a stale store read doesn't matter
        // because the per-node branch match is not an identity check.
        // Issue #1384 — iterate `nodeIds` and dereference through
        // `nodesById` rather than the removed `agentNodes` array.
        const { nodeIds, nodesById } = useAgentNodeStore.getState();
        for (const id of nodeIds) {
          const node = nodesById[id];
          if (node && node.mesh_id === activeMeshId && node.branch === pr.head_ref) {
            refreshOpenPrByPath(getNodeGitPath(node));
          }
        }
      }
      // Refetch so the merged PR drops out of the open list. Bumping
      // reloadKey aborts the in-flight effect (if any) and re-runs it
      // — see the useAsyncEffect deps at the top of this component.
      setReloadKey((k) => k + 1);
    } catch (e) {
      console.error('Failed to merge PR:', e);
      setMergeError((prev) => ({
        ...prev,
        [pr.number]: formatError(e),
      }));
    } finally {
      setMerging(null);
    }
  };

  // Read-only "View changes" (issue #421). Always available — the diff is
  // useful regardless of merge state, so we don't gate on `status.kind`.
  // `filePath: ''` opens the overlay in list mode (every file in the PR);
  // clicking a file there drills into a single-file patch view. `nodeId`
  // stays null because the PR's source branch may not exist locally; the
  // overlay's auto-close is mesh-scoped only.
  const handleViewChanges = (pr: GitHubPullRequest) => {
    if (activeMeshId === null) return;
    openDiff({
      filePath: '',
      rootPath: activeMeshPath ?? '',
      nodeId: null,
      meshId: activeMeshId,
      source: 'pr',
      prNumber: pr.number,
    });
  };

  return (
    <div className="flex flex-col h-full">
      {/* Toolbar mirrors GitIssuesTab. Manual Refresh on the left,
          "View on GitHub" link + Open/Closed toggle on the right. */}
      <ProbeToolbar
        trailing={
          <>
            {/* "View on GitHub" — opens the repo's PR list. `SafeLink`
                falls back to an inert <span> when the URL is empty
                (non-GitHub mesh), so the layout stays stable across
                meshes. */}
            <SafeLink
              url={pullsListUrl}
              ariaLabel="Open this repo's pull requests list on GitHub"
              title="Open this repo's pull requests list on GitHub"
              className="text-2xs font-medium text-accent-cyan hover:text-accent-cyan/80 transition-colors"
            >
              View on GitHub ↗
            </SafeLink>
            {/* Open / Closed segmented toggle */}
            <div className="flex shrink-0 rounded-md overflow-hidden border border-border-subtle">
              {(['open', 'closed'] as const).map((s) => (
                <button
                  key={s}
                  type="button"
                  onClick={() => setStateFilter(s)}
                  aria-pressed={stateFilter === s}
                  className={`px-2 py-0.5 text-2xs font-medium capitalize transition-colors ${
                    stateFilter === s
                      ? 'bg-accent-cyan/20 text-accent-cyan'
                      : 'text-text-muted hover:text-text-secondary hover:bg-bg-card'
                  }`}
                >
                  {s}
                </button>
              ))}
            </div>
          </>
        }
      >
        <RefreshControl
          onRefresh={retryListLoad}
          isRefreshing={loading && prs.length > 0}
          ariaLabel="Refresh pull requests"
        />
      </ProbeToolbar>

      <ProbeTabBody padding="p-3">
        {loading && prs.length === 0 ? (
          // First-load only: refreshes keep the prior list rendered.
          <LoadingState label="Loading pull requests..." />
        ) : error ? (
          <ErrorState title="Failed to load pull requests" detail={error} />
        ) : prs.length === 0 ? (
          <EmptyState label={`No ${stateFilter} pull requests`} />
        ) : (
          <div className="space-y-1">
            {prs.map((pr) => {
              const status = deriveMergeStatus(pr, pollExhausted);
              const isMerging = merging === pr.number;
              const isConfirming = confirming === pr.number;
              const rowError = mergeError[pr.number];
              const isSpawning = spawning === pr.number;
              const isDropdownOpen = openDropdown === pr.number;
              const rowSpawnError = spawnError[pr.number];
              const isExpanded = expanded.isExpanded(pr.number);
              return (
                <ProbeRow
                  key={pr.number}
                  dataAttr="pr"
                  rowKey={pr.number}
                  number={pr.number}
                  title={pr.title}
                  url={pr.url}
                  iconAriaLabel="Open pull request on GitHub"
                  isExpanded={isExpanded}
                  onToggle={() => expanded.toggle(pr.number)}
                  body={pr.body}
                  rightSlot={
                    // Three action groups, all gated to the open filter
                    // (closed PRs are read-only — no merge / spawn). The
                    // `onMouseDown stopPropagation` on each wrapper keeps
                    // the document-level mousedown click-outside handler
                    // from closing the provider picker mid-click. The
                    // rightSlot is a sibling of the clickable column, so
                    // button clicks here don't bubble to the row's
                    // `onToggle` — see `ProbeRow.tsx` for the layout.
                    <>
                      {/* Merge control — open PRs only. Icon-only button
                          (git-merge SVG) reclaims the 360px dock from
                          button text. `aria-label` carries the PR number
                          for screen readers; `title` is the hover tooltip.
                          In the confirm state we swap to a green check +
                          muted x — same colour semantics the text version
                          used (green = go, muted = dismiss). The Confirm
                          button is intentionally larger than Cancel via
                          `px-2.5` (vs `p-1.5`) so the eye lands on the
                          safe-looking affirmative. */}
                      {stateFilter === 'open' && (
                        <div
                          className="shrink-0 flex items-center gap-1"
                          onMouseDown={(e) => e.stopPropagation()}
                        >
                          {isMerging ? (
                            <span className="px-2 py-1 text-xs text-text-muted">Merging...</span>
                          ) : isConfirming ? (
                            <>
                              <button
                                type="button"
                                onClick={() => handleMerge(pr)}
                                aria-label={`Confirm squash merge of pull request #${pr.number}`}
                                title="Confirm squash merge"
                                className="inline-flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded-md bg-accent-green/15 text-accent-green hover:bg-accent-green/25 transition-colors"
                              >
                                <CheckIcon className="w-3.5 h-3.5" />
                                <span>Confirm</span>
                              </button>
                              <button
                                type="button"
                                onClick={() => setConfirming(null)}
                                aria-label={`Cancel merge of pull request #${pr.number}`}
                                title="Cancel"
                                className="p-1.5 rounded-md text-text-muted hover:text-text-secondary hover:bg-bg-card transition-colors"
                              >
                                <XIcon className="w-3.5 h-3.5" />
                              </button>
                            </>
                          ) : status.kind === 'mergeable' ? (
                            <button
                              type="button"
                              onClick={() => setConfirming(pr.number)}
                              disabled={merging !== null}
                              aria-label={`Merge pull request #${pr.number}`}
                              title="Merge pull request"
                              className="p-1.5 rounded-md bg-accent-cyan/10 text-accent-cyan hover:bg-accent-cyan/20 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                            >
                              <GitMergeIcon className="w-3.5 h-3.5" />
                            </button>
                          ) : status.kind === 'checking' ? (
                            <span className="px-2 py-1 text-2xs text-text-muted animate-pulse" title="GitHub hasn't computed mergeability yet — retrying automatically">Checking…</span>
                          ) : status.kind === 'unknown' ? (
                            <button
                              type="button"
                              onClick={retryUnknownRow}
                              aria-label={`Retry mergeability check for pull request #${pr.number}`}
                              title="GitHub hasn't reported mergeability — click to retry"
                              className="px-2 py-1 text-2xs rounded border border-dashed border-border-subtle text-text-muted hover:text-accent-cyan hover:border-accent-cyan/40 transition-colors" /* allow-bare-rounded */
                            >
                              Unknown
                            </button>
                          ) : (
                            <span className="px-2 py-1 text-2xs rounded bg-bg-card text-text-muted" title="This pull request can't be merged">{/* allow-bare-rounded */}
                              {status.label}
                            </span>
                          )}
                        </div>
                      )}

                      {/* Canonical `+ ▾` Spawn Menu cluster (ADR-0016 §2) —
                          the same cluster the sidebar's `NodeCreationForm`
                          and the issues-probe row use. Open PRs only;
                          closed PRs are read-only. The row is disabled while
                          any spawn is in flight (busy state shared across
                          rows); `isSpawning` flips the `+` to "Spawning…"
                          while THIS row's stage-2 IPC is in flight. The
                          outer `flex shrink-0` wrapper carries
                          `onMouseDown` stopPropagation so a click on the
                          spawn cluster doesn't toggle the row's expand
                          state (mirrors the issue-tab wrapper). */}
                      {stateFilter === 'open' && (
                        <div
                          className="flex shrink-0"
                          onMouseDown={(e) => e.stopPropagation()}
                        >
                          <SpawnButtonCluster
                            providers={providerList}
                            // Issue #1264 — surface prefix keeps
                            // this menu's `data-dropdown-for`
                            // from colliding with a mesh-, node-,
                            // or issue-keyed menu on the same
                            // numeric id.
                            dropdownKey={dropdownId('pr', pr.number)}
                            isOpen={isDropdownOpen}
                            onToggleDropdown={() =>
                              setOpenDropdown(isDropdownOpen ? null : pr.number)
                            }
                            onSpawnDefault={() => handleDefaultSpawn(pr)}
                            onSelectProvider={(providerId) => handleSpawn(pr, providerId)}
                            disabled={spawning !== null}
                            isSpawning={isSpawning}
                          />
                        </div>
                      )}

                      {/* Read-only "View changes" (issue #421). Always
                          available for any state filter — the diff is
                          useful on closed PRs too (review a merged change,
                          compare with a rebase). `onMouseDown`
                          stopPropagation mirrors the merge control so a
                          future click-outside picker doesn't swallow the
                          click. Icon-only (file-text SVG) — the visible
                          "View changes" text ate ~80px of the 360px dock.
                          Hover tooltip + aria-label preserve the semantics
                          for sighted and AT users. */}
                      <div
                        className="shrink-0 flex items-center"
                        onMouseDown={(e) => e.stopPropagation()}
                      >
                        <button
                          type="button"
                          onClick={() => handleViewChanges(pr)}
                          aria-label={`View changes in PR #${pr.number}`}
                          title="Open the PR's diff in the center overlay"
                          className="p-1.5 rounded-md text-text-secondary hover:text-accent-cyan hover:bg-accent-cyan/10 transition-colors"
                        >
                          <FileTextIcon className="w-3.5 h-3.5" />
                        </button>
                      </div>
                    </>
                  }
                  belowSlot={
                    // Inline merge / spawn error rows (issues #419, #420,
                    // #444). Rendered below the title row, outside the
                    // clickable column — clicking an error message no
                    // longer toggles expand. This is a subtle UX
                    // improvement: the user reading an error probably
                    // doesn't expect their next click to collapse the
                    // body. The error text is still findable via
                    // `findByText` and tests still pass (no test covered
                    // the "clicking the error toggles expand" behaviour,
                    // which was almost certainly accidental anyway).
                    <>
                      {rowError && (
                        <p className="text-2xs text-status-error mt-1 max-w-[260px]">{rowError}</p>
                      )}
                      {rowSpawnError && (
                        <p className="text-2xs text-status-error mt-1 max-w-[260px]">{rowSpawnError}</p>
                      )}
                    </>
                  }
                />
              );
            })}
          </div>
        )}
      </ProbeTabBody>
    </div>
  );
}
