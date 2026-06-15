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
 */

import { useState, useEffect, useCallback } from 'react';
import {
  getRepoPulls,
  getPrMergeability,
  mergePr,
  type GitHubPullRequest,
  type PrMergeability,
} from '../../lib/tauri';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useUIStore } from '../../stores/uiStore';

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

  const load = useCallback(async (signal: { cancelled: boolean }) => {
    if (activeMeshId === null) return;
    setLoading(true);
    setError(null);
    setMergeability({});
    setConfirming(null);
    setMergeError({});
    try {
      const result = await getRepoPulls(activeMeshId, stateFilter);
      // The mesh / filter could have changed mid-flight — drop a stale result.
      if (signal.cancelled) return;
      setPrs(result);
    } catch (e) {
      if (signal.cancelled) return;
      console.error('Failed to load pull requests:', e);
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      if (!signal.cancelled) setLoading(false);
    }
  }, [activeMeshId, stateFilter]);

  useEffect(() => {
    if (activeMeshId === null) return;
    const signal = { cancelled: false };
    load(signal);
    return () => {
      signal.cancelled = true;
    };
  }, [activeMeshId, stateFilter, load]);

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
  useEffect(() => {
    if (activeMeshId === null || stateFilter !== 'open') return;
    let cancelled = false;
    // Per-PR retry timers + attempt counts. Maps are owned by this effect
    // instance; cleanup discards them and clears the timers.
    const retryTimers = new Map<number, ReturnType<typeof setTimeout>>();
    const attempts = new Map<number, number>();
    const MAX_MERGEABILITY_ATTEMPTS = 4;
    const BASE_RETRY_DELAY_MS = 1500;

    const probe = (pr: GitHubPullRequest) => {
      getPrMergeability(activeMeshId, pr.number)
        .then((info) => {
          if (cancelled) return;
          setMergeability((prev) => ({ ...prev, [pr.number]: info }));
          // Still computing — schedule a bounded retry.
          if (info.mergeable === null) {
            const attempt = (attempts.get(pr.number) ?? 0) + 1;
            if (attempt < MAX_MERGEABILITY_ATTEMPTS) {
              attempts.set(pr.number, attempt);
              const delay = BASE_RETRY_DELAY_MS * attempt;
              const timer = setTimeout(() => {
                retryTimers.delete(pr.number);
                if (cancelled) return;
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
      cancelled = true;
      for (const timer of retryTimers.values()) clearTimeout(timer);
      retryTimers.clear();
    };
  }, [prs, stateFilter, activeMeshId]);

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
      // Refetch so the merged PR drops out of the open list.
      load({ cancelled: false });
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
      <div className="px-3 py-1.5 border-b border-border-subtle flex items-center justify-between gap-2">
        {activeMeshPath ? (
          <p className="text-[10px] text-text-muted truncate flex-1" title={activeMeshPath}>{activeMeshPath}</p>
        ) : (
          <span className="flex-1" />
        )}
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
              return (
                <div
                  key={pr.number}
                  className="flex items-start gap-2 px-2 py-2 rounded hover:bg-bg-card transition-colors"
                >
                  <div className="flex-1 min-w-0">
                    <div>
                      <span className="text-xs text-accent-cyan font-mono">#{pr.number}</span>
                      <span className="text-sm text-text-primary ml-2">{pr.title}</span>
                    </div>
                    {pr.body && (
                      <p className="text-[10px] text-text-muted mt-1 line-clamp-2">{pr.body}</p>
                    )}
                    {rowError && (
                      <p className="text-[10px] text-red-400 mt-1 max-w-[260px]">{rowError}</p>
                    )}
                  </div>

                  {/* Merge control — open PRs only; closed PRs are read-only. */}
                  {stateFilter === 'open' && (
                    <div className="shrink-0 flex items-center gap-1" onMouseDown={(e) => e.stopPropagation()}>
                      {isMerging ? (
                        <span className="px-2 py-1 text-xs text-text-muted">Merging...</span>
                      ) : isConfirming ? (
                        <>
                          <button
                            onClick={() => handleMerge(pr)}
                            className="px-2 py-1 text-xs font-medium rounded bg-accent-green/15 text-accent-green hover:bg-accent-green/25 transition-colors"
                          >
                            Merge?
                          </button>
                          <button
                            onClick={() => setConfirming(null)}
                            className="px-2 py-1 text-xs font-medium rounded text-text-muted hover:text-text-secondary transition-colors"
                          >
                            Cancel
                          </button>
                        </>
                      ) : status.kind === 'mergeable' ? (
                        <button
                          onClick={() => setConfirming(pr.number)}
                          disabled={merging !== null}
                          className="px-2.5 py-1 text-xs font-medium rounded bg-accent-cyan/10 text-accent-cyan hover:bg-accent-cyan/20 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
                        >
                          Merge
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

                  {/* Read-only "View changes" (issue #421). Always available
                      for any state filter — the diff is useful on closed PRs
                      too (review a merged change, compare with a rebase).
                      `onMouseDown` stopPropagation mirrors the merge control
                      so a future click-outside picker (issue #373's dock
                      pattern) doesn't swallow the click. */}
                  <div className="shrink-0 flex items-center" onMouseDown={(e) => e.stopPropagation()}>
                    <button
                      type="button"
                      onClick={() => handleViewChanges(pr)}
                      aria-label={`View changes in PR #${pr.number}`}
                      title="Open the PR's diff in the center overlay"
                      className="px-2.5 py-1 text-xs font-medium rounded text-text-secondary hover:text-accent-cyan hover:bg-accent-cyan/10 transition-colors"
                    >
                      View changes
                    </button>
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
