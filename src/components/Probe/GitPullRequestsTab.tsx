/**
 * GitPullRequestsTab — the Probe Panel's 🔀 tab body.
 *
 * Sibling of `GitIssuesTab`: same dock-supplied header / close button, same
 * `useProbeContext` mesh scoping, same loading / error / empty skeleton. Lists
 * the active mesh's pull requests (open by default, with a toggle to closed)
 * and lets the user squash-merge a mergeable open PR straight from the panel.
 *
 * Mergeability is a two-call problem
 * ----------------------------------
 * GitHub's `/pulls` list endpoint does NOT return `mergeable` — only the
 * single-PR detail endpoint does, and even there it's `null` while GitHub
 * computes the merge asynchronously. So the list loads fast, then each open,
 * non-draft PR is enriched in parallel via `getPrMergeability`. Until that
 * resolves the row shows "Checking…"; a draft PR is flagged without any
 * detail call. If GitHub returns `null` (still computing) the enrichment
 * effect schedules a bounded retry so the row doesn't get stuck on
 * "Checking…" forever — see issue #419 and the inline note above the
 * enrichment useEffect.
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
 * stored in `branch` (and `source_pr` set), then stage-2 fetches
 * `origin/<head_ref>` and cuts the worktree from it — so the agent lands on
 * the same commits the PR is built from (worktree adoption, #36 follow-up).
 * The dock stays open after a successful spawn (matches the issue-tab
 * behaviour, see memory buildmesh-spawn-from-probe-keeps-dock-open).
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

import { useState } from 'react';
import { openUrl } from '@tauri-apps/plugin-opener';
import {
  getRepoPulls,
  getPrMergeability,
  mergePr,
  createPrNode,
  startNodeBackground,
  listProviders,
  type GitHubPullRequest,
  type PrMergeability,
} from '../../lib/tauri';
import { useMeshStore } from '../../stores/meshStore';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useUIStore } from '../../stores/uiStore';
import { ProviderDropdown, colorClassForProvider, type ProviderEntry } from '../Sidebar/ProviderDropdown';

type StateFilter = 'open' | 'closed';

/** Derived merge readiness for one open PR. */
type MergeStatus =
  | { kind: 'checking' }
  | { kind: 'mergeable' }
  | { kind: 'blocked'; label: string };

// Flag wording for a non-mergeable PR, keyed by GitHub's `mergeable_state`.
// Module-level so it isn't rebuilt on every `deriveMergeStatus` call.
const BLOCKED_WORDING: Record<string, string> = {
  dirty: 'Conflicts',
  blocked: 'Blocked',
  behind: 'Behind',
};

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
 * Turn a PR + its (maybe-missing) mergeability into a display status.
 * `draft` short-circuits to blocked without a detail call. An absent map
 * entry, or a `mergeable: null` (GitHub still computing), is "checking".
 */
function deriveMergeStatus(
  pr: GitHubPullRequest,
  info: PrMergeability | undefined,
): MergeStatus {
  if (pr.draft) return { kind: 'blocked', label: 'Draft' };
  if (info === undefined || info.mergeable === null) return { kind: 'checking' };
  if (info.mergeable === true) return { kind: 'mergeable' };
  // mergeable === false — word the flag from GitHub's mergeable_state.
  return { kind: 'blocked', label: BLOCKED_WORDING[info.mergeable_state] ?? 'Conflicts' };
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
  // Mergeability keyed by PR number. Absence = not fetched yet ("checking");
  // a fetched value may itself carry `mergeable: null` (still computing).
  const [mergeability, setMergeability] = useState<Record<number, PrMergeability>>({});
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
  const [providerList, setProviderList] = useState<ProviderEntry[]>([]);
  // Per-row expand state (issue #461) — Set keyed by PR number so two
  // long PRs can stay expanded side-by-side. Reset on every load
  // below (mesh + open/closed filter both change the visible set of
  // PR numbers, so the prior Set would either no-op or re-open a
  // different row).
  const [expanded, setExpanded] = useState<Set<number>>(new Set());
  const toggleExpanded = (n: number) =>
    setExpanded((prev) => {
      const next = new Set(prev);
      if (next.has(n)) next.delete(n);
      else next.add(n);
      return next;
    });
  // Bump to force the load effect to re-run — the previous run's signal
  // is aborted by useAsyncEffect's cleanup, so any in-flight getRepoPulls
  // drops its setState instead of clobbering the new run's result. Used
  // by `handleMerge` to refetch after a successful squash-merge (issue #349).
  const [reloadKey, setReloadKey] = useState(0);

  useAsyncEffect((signal) => {
    if (activeMeshId === null) return;
    setLoading(true);
    setError(null);
    setMergeability({});
    setConfirming(null);
    setMergeError({});
    // PR numbers don't carry across mesh/filter changes (issue #461).
    setExpanded(new Set());
    (async () => {
      try {
        const result = await getRepoPulls(activeMeshId, stateFilter);
        // The mesh / filter could have changed mid-flight — drop a stale result.
        if (signal.aborted) return;
        setPrs(result);
      } catch (e) {
        if (signal.aborted) return;
        console.error('Failed to load pull requests:', e);
        setError(e instanceof Error ? e.message : String(e));
      } finally {
        if (!signal.aborted) setLoading(false);
      }
    })();
  }, [activeMeshId, stateFilter, reloadKey]);

  // Enrich each open, non-draft PR with its mergeability in parallel. Closed
  // PRs can't be merged, and drafts are flagged without a call, so both skip.
  // Keyed on the list (not the `mergeability` map) so it runs exactly once per
  // load — depending on the map would let the first probe's setState cancel the
  // siblings' in-flight callbacks and force needless refetches. `load` already
  // clears the map, so there's nothing stale to guard against here.
  //
  // Re-poll on `mergeable: null` (issue #419)
  // ----------------------------------------
  // GitHub computes the merge asynchronously, so the detail endpoint can return
  // `mergeable: null` for a few seconds. We schedule a bounded retry — first
  // retry after 1.5s, then 3s, 4.5s — and give up after `MAX_MERGEABILITY_ATTEMPTS`
  // total (1 initial + 3 retries, ~9s). Past that, "Checking…" is the best we
  // can do without spamming GitHub; the next list reload retries from scratch.
  // Cleanup clears every pending retry timer so a filter toggle / mesh change /
  // unmount tears down the whole chain at once.
  useAsyncEffect((signal) => {
    if (activeMeshId === null || stateFilter !== 'open') return;
    // Per-PR retry timers + attempt counts. Maps are owned by this effect
    // instance; cleanup discards them and clears the timers.
    const retryTimers = new Map<number, ReturnType<typeof setTimeout>>();
    const attempts = new Map<number, number>();
    const MAX_MERGEABILITY_ATTEMPTS = 4;
    const BASE_RETRY_DELAY_MS = 1500;

    const probe = (pr: GitHubPullRequest) => {
      getPrMergeability(activeMeshId, pr.number)
        .then((info) => {
          if (signal.aborted) return;
          setMergeability((prev) => ({ ...prev, [pr.number]: info }));
          // Still computing — schedule a bounded retry.
          if (info.mergeable === null) {
            const attempt = (attempts.get(pr.number) ?? 0) + 1;
            if (attempt < MAX_MERGEABILITY_ATTEMPTS) {
              attempts.set(pr.number, attempt);
              const delay = BASE_RETRY_DELAY_MS * attempt;
              const timer = setTimeout(() => {
                retryTimers.delete(pr.number);
                if (signal.aborted) return;
                probe(pr);
              }, delay);
              retryTimers.set(pr.number, timer);
            }
            // else: exhausted retries; row stays in "Checking…" (matches
            // pre-fix behaviour and avoids spamming GitHub).
          }
        })
        .catch((e) => {
          // A failed mergeability probe leaves the row in "Checking…" rather
          // than falsely claiming conflicts; the next list reload retries.
          console.error(`mergeability probe failed for PR #${pr.number}:`, e);
        });
    };

    prs
      .filter((pr) => !pr.draft)
      .forEach((pr) => {
        attempts.set(pr.number, 0);
        probe(pr);
      });

    return () => {
      for (const timer of retryTimers.values()) clearTimeout(timer);
      retryTimers.clear();
    };
  }, [prs, stateFilter, activeMeshId]);

  // Fetch the provider list once at mount (issue #420, mirrors
  // `GitIssuesTab`). Platform filtering is enforced server-side via
  // `AgentProvider::available_on()`. Re-fetching on every render would be
  // wasteful, and the list is stable for the lifetime of a session.
  useAsyncEffect((signal) => {
    listProviders()
      .then((backendProviders) => {
        if (signal.aborted) return;
        setProviderList(
          backendProviders.map((p) => ({ id: p.id, label: p.label, color: colorClassForProvider(p.id) })),
        );
      })
      .catch((err) => console.error('listProviders failed:', err));
  }, []);

  // Close the provider dropdown when clicking outside it. The dropdown
  // container carries a `data-dropdown-for` attribute set to the PR number,
  // matching the issue-tab pattern (memory:
  // feedback-probe-tab-test-and-jsdoc-gotchas — mousedown vs click race).
  // Issue #492 — shared `useClickOutside` hook; the scoped selector lives
  // in the hook so a future caller cannot reintroduce the loose-selector
  // drift that #492 fixed in Sidebar.
  useClickOutside(openDropdown, () => setOpenDropdown(null));

  // Two-stage PR spawn (issue #420, extended by #443 for fork PRs) —
  // mirrors the issue-spawn flow in `GitIssuesTab`. Stage-1
  // (`createPrNode`) is the fast DB-only IPC (~20ms) that returns a
  // `pending` node + the prefill; stage-2 (`startNodeBackground`) does the
  // slow work (git fetch <remote> <head_ref>, worktree create off the
  // head ref, PTY spawn) in the background and the store flips the node
  // to `running` / `error` via `node-spawn-completed` /
  // `node-spawn-failed` events. We deliberately do NOT toggle the probe —
  // the dock stays open so the user can fire off another PR without
  // re-opening the context menu (matches the issue-tab contract; see
  // memory buildmesh-spawn-from-probe-keeps-dock-open).
  //
  // For fork PRs (#443) the stage-2 path adds the fork as a remote and
  // fetches the head ref from there instead of from `origin`. The fork
  // fields come from the GitHub list response (`head_repo_owner` +
  // `head_repo_clone_url` on `GitHubPullRequest`); for same-repo PRs both
  // are empty strings and the backend takes the #420 origin-fetch path.
  const handleSpawn = async (pr: GitHubPullRequest, providerId: string) => {
    if (activeMeshId === null) return;
    setSpawning(pr.number);
    setSpawnError((prev) => {
      const next = { ...prev };
      delete next[pr.number];
      return next;
    });
    try {
      const draft = await createPrNode(
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
      // Dispatch stage-2 BEFORE clearing the busy state so the
      // fire-and-forget IPC is on the wire before the same-row button
      // re-enables for another click.
      startNodeBackground(draft.id, draft.prefill);
      setSpawning(null);
    } catch (e) {
      console.error('Failed to spawn PR agent:', e);
      setSpawnError((prev) => ({
        ...prev,
        [pr.number]: e instanceof Error ? e.message : String(e),
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
      const draft = await createPrNode(
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
      // Same ordering as `handleSpawn`: stage-2 IPC first, then clear the
      // busy state. Dock stays open (see handleSpawn).
      startNodeBackground(draft.id, draft.prefill);
      setSpawning(null);
    } catch (e) {
      console.error('Failed to spawn PR agent:', e);
      setSpawnError((prev) => ({
        ...prev,
        [pr.number]: e instanceof Error ? e.message : String(e),
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
      // Refetch so the merged PR drops out of the open list. Bumping
      // reloadKey aborts the in-flight effect (if any) and re-runs it
      // — see the useAsyncEffect deps at the top of this component.
      setReloadKey((k) => k + 1);
    } catch (e) {
      console.error('Failed to merge PR:', e);
      setMergeError((prev) => ({
        ...prev,
        [pr.number]: e instanceof Error ? e.message : String(e),
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
      <div className="px-3 py-2 border-b border-border-subtle flex items-center justify-end gap-2">
        {/* Open / Closed segmented toggle */}
        <div className="flex shrink-0 rounded overflow-hidden border border-border-subtle">
          {(['open', 'closed'] as const).map((s) => (
            <button
              key={s}
              type="button"
              onClick={() => setStateFilter(s)}
              aria-pressed={stateFilter === s}
              className={`px-2 py-0.5 text-[10px] font-medium capitalize transition-colors ${
                stateFilter === s
                  ? 'bg-accent-cyan/20 text-accent-cyan'
                  : 'text-text-muted hover:text-text-secondary'
              }`}
            >
              {s}
            </button>
          ))}
        </div>
      </div>

      <div className="flex-1 overflow-y-auto p-2">
        {loading ? (
          <div className="flex flex-col items-center justify-center py-8 gap-3">
            <div className="animate-spin w-5 h-5 border border-accent-cyan border-t-transparent rounded-full" />
            <span className="text-xs text-text-muted">Loading pull requests...</span>
          </div>
        ) : error ? (
          <div className="flex flex-col items-center justify-center py-8">
            <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" className="text-red-400 mb-2">
              <circle cx="12" cy="12" r="10"/>
              <line x1="15" y1="9" x2="9" y2="15"/>
              <line x1="9" y1="9" x2="15" y2="15"/>
            </svg>
            <span className="text-xs text-red-400">Failed to load pull requests</span>
            <span className="text-[10px] text-text-muted mt-1 max-w-[280px] text-center">{error}</span>
          </div>
        ) : prs.length === 0 ? (
          <div className="flex flex-col items-center justify-center py-8">
            <svg width="32" height="32" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" className="text-text-muted mb-2">
              <circle cx="12" cy="12" r="10"/>
              <line x1="12" y1="8" x2="12" y2="12"/>
              <line x1="12" y1="16" x2="12.01" y2="16"/>
            </svg>
            <span className="text-xs text-text-muted">No {stateFilter} pull requests</span>
          </div>
        ) : (
          <div className="space-y-1">
            {prs.map((pr) => {
              const status = deriveMergeStatus(pr, mergeability[pr.number]);
              const isMerging = merging === pr.number;
              const isConfirming = confirming === pr.number;
              const rowError = mergeError[pr.number];
              const isSpawning = spawning === pr.number;
              const isDropdownOpen = openDropdown === pr.number;
              const rowSpawnError = spawnError[pr.number];
              const isExpanded = expanded.has(pr.number);
              // Local alias so the two anchor gates below stay in sync
              // (issue #461 empty-URL guard, see memory).
              const url = pr.url;
              return (
                <div
                  key={pr.number}
                  data-pr-row={pr.number}
                  className="flex flex-col gap-1 px-2 py-2 rounded hover:bg-bg-card transition-colors"
                >
                  <div className="flex items-start gap-2">
                    {/* Title <a> uses onClick stopPropagation so navigating
                        to GitHub doesn't also toggle expand (issue #461). */}
                    <div
                      className="flex-1 min-w-0 cursor-pointer"
                      onClick={() => toggleExpanded(pr.number)}
                    >
                      {/* `min-w-0` on this nested flex + `min-w-0 flex-1`
                          on the title <a>/<span> is required for the
                          `truncate` class to actually take effect. Without
                          it, a long PR title (PRs often have multi-clause
                          titles) overflows the flex parent and wraps to
                          multiple lines, visually colliding with the
                          Merge/Spawn/View-changes buttons to the right.
                          The outer wrapper already has `flex-1 min-w-0` so
                          the title area is bounded; the inner flex must
                          also be `min-w-0` for its children to shrink
                          below their intrinsic content width. */}
                      <div className="flex items-center gap-1 min-w-0">
                        <span
                          aria-hidden
                          className={
                            'text-text-muted text-[10px] w-3 text-center shrink-0 transition-transform ' +
                            (isExpanded ? 'rotate-90' : '')
                          }
                        >
                          ▸
                        </span>
                        <span className="text-xs text-accent-cyan font-mono">#{pr.number}</span>
                        {url ? (
                          // href/target/rel keep the link right-clickable,
                          // keyboard-activatable, and AT-friendly. The onClick
                          // routes through `openUrl` because the Tauri 2 WebView
                          // is not a browser — `target="_blank"` is silently
                          // dropped without `core:webview:allow-create-webview-window`
                          // (which we don't grant). `e.preventDefault()` stops
                          // the dead default action; `e.stopPropagation()` keeps
                          // the row's expand-toggle out of the way. Mirrors the
                          // PR-chip pattern in `GridNodeHeader.tsx:145` and the
                          // web-links addon in `TerminalRegistry.ts:249`.
                          <a
                            href={url}
                            target="_blank"
                            rel="noopener noreferrer"
                            onClick={(e) => {
                              e.preventDefault();
                              e.stopPropagation();
                              openUrl(url).catch(console.error);
                            }}
                            className="text-sm text-text-primary hover:underline ml-1 truncate min-w-0 flex-1"
                            title="Open on GitHub"
                          >
                            {pr.title}
                          </a>
                        ) : (
                          // Defensive guard: see memory
                          // buildmesh-empty-url-frontend-guard. A bare
                          // <a href=""> would self-navigate the WebView.
                          <span className="text-sm text-text-primary ml-1 truncate min-w-0 flex-1">
                            {pr.title}
                          </span>
                        )}
                        {url && (
                          // See title <a> above — Tauri 2 needs openUrl routing.
                          <a
                            href={url}
                            target="_blank"
                            rel="noopener noreferrer"
                            onClick={(e) => {
                              e.preventDefault();
                              e.stopPropagation();
                              openUrl(url).catch(console.error);
                            }}
                            aria-label="Open pull request on GitHub"
                            className="text-text-muted hover:text-accent-cyan transition-colors text-[11px] shrink-0"
                            title="Open on GitHub"
                          >
                            ↗
                          </a>
                        )}
                      </div>
                      {pr.body && (
                        isExpanded ? (
                          <div
                            data-pr-body-expanded
                            className="mt-1 max-h-48 overflow-y-auto text-[10px] text-text-muted whitespace-pre-wrap break-words"
                          >
                            {pr.body}
                          </div>
                        ) : (
                          <p className="text-[10px] text-text-muted mt-1 line-clamp-2">{pr.body}</p>
                        )
                      )}
                      {rowError && (
                        <p className="text-[10px] text-red-400 mt-1 max-w-[260px]">{rowError}</p>
                      )}
                      {rowSpawnError && (
                        <p className="text-[10px] text-red-400 mt-1 max-w-[260px]">{rowSpawnError}</p>
                      )}
                    </div>

                  {/* Merge control — open PRs only; closed PRs are read-only.
                      The action button is icon-only (git-merge SVG) to keep
                      the 360px dock from being eaten by button text. The
                      `aria-label` carries the PR number so screen readers
                      announce "Merge pull request #201" and `title` gives
                      mouse users a hover tooltip — the discoverability that
                      used to come from visible text. In the confirm state
                      we swap to a green check + muted x; the same colour
                      semantics the text version used (green = go, muted =
                      dismiss). The Confirm button is intentionally larger
                      than Cancel via `px-2.5` (vs `p-1.5`) so the eye lands
                      on the safe-looking affirmative — destructive-style
                      confirm would invert this. */}
                  {stateFilter === 'open' && (
                    <div className="shrink-0 flex items-center gap-1" onMouseDown={(e) => e.stopPropagation()}>
                      {isMerging ? (
                        <span className="px-2 py-1 text-xs text-text-muted">Merging...</span>
                      ) : isConfirming ? (
                        <>
                          <button
                            type="button"
                            onClick={() => handleMerge(pr)}
                            aria-label={`Confirm squash merge of pull request #${pr.number}`}
                            title="Confirm squash merge"
                            className="inline-flex items-center gap-1.5 px-2.5 py-1 text-xs font-medium rounded bg-accent-green/15 text-accent-green hover:bg-accent-green/25 transition-colors"
                          >
                            <CheckIcon className="w-3.5 h-3.5" />
                            <span>Confirm</span>
                          </button>
                          <button
                            type="button"
                            onClick={() => setConfirming(null)}
                            aria-label={`Cancel merge of pull request #${pr.number}`}
                            title="Cancel"
                            className="p-1.5 rounded text-text-muted hover:text-text-secondary hover:bg-bg-card transition-colors"
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
                          className="p-1.5 rounded bg-accent-cyan/10 text-accent-cyan hover:bg-accent-cyan/20 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                        >
                          <GitMergeIcon className="w-3.5 h-3.5" />
                        </button>
                      ) : status.kind === 'checking' ? (
                        <span className="px-2 py-1 text-[10px] text-text-muted">Checking…</span>
                      ) : (
                        <span className="px-2 py-1 text-[10px] rounded bg-bg-card text-text-muted" title="This pull request can't be merged">
                          {status.label}
                        </span>
                      )}
                    </div>
                  )}

                  {/* Split spawn button (issue #420) — open PRs only;
                      closed PRs are read-only. Mirrors the issue-tab
                      pattern: primary "Spawn" uses the mesh's default
                      provider, the `▾` half opens a picker that bypasses
                      the default. The row is disabled while any spawn
                      is in flight (busy state shared across rows). The
                      `data-dropdown-for` attribute feeds the click-outside
                      handler in the spawn-effects block above. */}
                  {stateFilter === 'open' && (
                    <div
                      className="relative flex shrink-0"
                      data-dropdown-for={pr.number}
                      onMouseDown={(e) => e.stopPropagation()}
                    >
                      {isSpawning ? (
                        <button
                          disabled
                          className="px-2.5 py-1 text-xs font-medium rounded bg-accent-cyan/10 text-text-muted disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                        >
                          Spawning...
                        </button>
                      ) : (
                        <>
                          <button
                            onClick={() => handleDefaultSpawn(pr)}
                            disabled={spawning !== null}
                            className="px-2.5 py-1 text-xs font-medium rounded-l bg-accent-cyan/10 text-accent-cyan hover:bg-accent-cyan/20 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                          >
                            Spawn
                          </button>
                          <button
                            onClick={() => setOpenDropdown(isDropdownOpen ? null : pr.number)}
                            disabled={spawning !== null}
                            className="px-1.5 py-1 text-xs font-medium rounded-r border-l border-accent-cyan/20 bg-accent-cyan/10 text-accent-cyan hover:bg-accent-cyan/20 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                            title="Choose provider"
                          >
                            ▾
                          </button>
                          {isDropdownOpen && (
                            <ProviderDropdown
                              meshId={pr.number}
                              providers={providerList}
                              onSelect={(providerId) => handleSpawn(pr, providerId)}
                            />
                          )}
                        </>
                      )}
                    </div>
                  )}

                  {/* Read-only "View changes" (issue #421). Always available
                      for any state filter — the diff is useful on closed PRs
                      too (review a merged change, compare with a rebase).
                      `onMouseDown` stopPropagation mirrors the merge control
                      so a future click-outside picker (issue #373's dock
                      pattern) doesn't swallow the click. Icon-only (file-text
                      SVG) — the visible "View changes" text ate ~80px of the
                      360px dock. Hover tooltip + aria-label preserve the
                      semantics for sighted and AT users. */}
                  <div className="shrink-0 flex items-center" onMouseDown={(e) => e.stopPropagation()}>
                    <button
                      type="button"
                      onClick={() => handleViewChanges(pr)}
                      aria-label={`View changes in PR #${pr.number}`}
                      title="Open the PR's diff in the center overlay"
                      className="p-1.5 rounded text-text-secondary hover:text-accent-cyan hover:bg-accent-cyan/10 transition-colors"
                    >
                      <FileTextIcon className="w-3.5 h-3.5" />
                    </button>
                  </div>
                  </div>
                </div>
              );
            })}
          </div>
        )}
      </div>
    </div>
  );
}
