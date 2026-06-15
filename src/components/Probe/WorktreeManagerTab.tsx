/**
 * WorktreeManagerTab — the Probe Panel's 🌳 Worktree Manager tab (issue #377).
 *
 * Ports the Git-maintenance surface that used to live at the bottom of the
 * legacy `MeshPropertiesPanel` (the `<BranchesWorktreesSection>`) into the
 * unified Probe. The new tab is a "lifted sections" component — it drops the
 * legacy collapsible header (the probe already supplies the surface chrome)
 * and the always-visible one-liner (the probe header shows the tab name).
 *
 * Three concerns, one body:
 *   1. **Health recovery** — drift detection, base-branch hostage, and the
 *      one-click Restore / Free buttons. The shared `useMeshHealth` and
 *      `useMeshRecovery` hooks are the source of truth (the sidebar's `!`
 *      badge reads from the same hook, so the two cannot disagree about
 *      the mesh's state).
 *   2. **Branch / worktree prune** — list local branches and worktrees per
 *      repo (a Mesh can include nested repos), with a "recommended"
 *      selection helper for merged + orphan + clean branches and stale
 *      worktrees. Delete goes through a `ConfirmDialog`.
 *   3. **Remote-tracking prune** — per-repo button to drop local refs to
 *      remote branches whose upstream is gone.
 *
 * The IPC seam (ADR-0010) is observed: every `invoke` goes through
 * `src/lib/tauri.ts` (`getGitPruneInfo`, `deleteBranches`, `deleteWorktrees`,
 * `pruneRemoteTracking`). The drift test at
 * `tests/unit/tauri-ipc-seam.test.ts` fails on a new component that
 * imports raw `invoke`, so this stays in the ratchet.
 *
 * Reactivity: the prune info is fetched on mount and after every delete /
 * free / restore (the recovery hook invalidates both health + prune on
 * success). The health block re-renders whenever the shared cache updates,
 * including on GIT_CHANGED events from the file-watcher.
 */

import { useCallback, useMemo, useState } from 'react';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useMeshHealth } from '../../hooks/useMeshHealth';
import { useMeshRecovery } from '../../hooks/useMeshRecovery';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
import {
  deleteBranches,
  deleteWorktrees,
  getGitPruneInfo,
  pruneRemoteTracking,
  type BranchInfo,
  type GitRepoPruneInfo,
  type HoldingWorktree,
  type MeshHealth,
  type WorktreeInfo,
} from '../../lib/tauri';

const Badge = ({ color, text, title }: { color: string; text: string; title?: string }) => (
  <span
    title={title}
    className={`px-1 py-px rounded text-[9px] font-medium leading-none ${color}`}
  >
    {text}
  </span>
);

function formatDate(iso: string | null): string {
  if (!iso) return '';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return '';
  return d.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
}

// Composite keys keep selection unambiguous across multiple repos (a Mesh
// can include nested git repos — each contributes its own branch list).
const branchKey = (repo: string, name: string) => `b:${repo}::${name}`;
const worktreeKey = (path: string) => `w:${path}`;

// "Safe to prune" recommendation. A branch is recommended when it isn't the
// current HEAD, has nothing uncommitted to lose, and is either fully merged
// into main or orphaned (its upstream remote branch is gone). A worktree is
// recommended when its branch no longer exists (stale) and no agent is using
// it. Mirrors the legacy section's logic.
const isRecommendedBranch = (b: BranchInfo) =>
  !b.is_head && !b.has_uncommitted && (b.is_merged_into_main === true || b.is_orphan);

const isRecommendedWorktree = (w: WorktreeInfo) => !w.is_active && w.is_stale;

export function WorktreeManagerTab() {
  const { activeMeshId, activePath } = useProbeContext();

  // The health hook subscribes to the shared cache (used by the sidebar's
  // `!` badge too) and refetches on GIT_CHANGED / focus. The prune info is
  // local to this tab — only it cares about the full list, so it doesn't
  // share a cache.
  const { health, refresh: refreshHealth } = useMeshHealth(activeMeshId, activePath);

  const [repos, setRepos] = useState<GitRepoPruneInfo[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [confirming, setConfirming] = useState(false);
  const [deleting, setDeleting] = useState(false);

  // Single prune-fetch body. Mount, mesh-switch, manual Refresh, and
  // post-recovery all route through `load`. The function returns the
  // promise so callers that need to sequence AFTER the refresh (e.g.
  // `handleDelete` wants the partial-failure error to land on top of
  // `load`'s own `setError(null)`) can `await load()`. `useAsyncEffect`
  // wraps the same call to gate the setStates behind `signal.aborted`,
  // so a rapid Refresh click while the previous fetch is still pending
  // can't clobber state with a stale response.
  const load = useCallback(() => {
    if (activeMeshId === null) return Promise.resolve();
    setLoading(true);
    setError(null);
    setSelected(new Set());
    return getGitPruneInfo(activeMeshId)
      .then((data) => {
        setRepos(data);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, [activeMeshId]);

  useAsyncEffect(
    (signal) => {
      if (activeMeshId === null) return;
      setLoading(true);
      setError(null);
      setSelected(new Set());
      getGitPruneInfo(activeMeshId)
        .then((data) => {
          if (!signal.aborted) {
            setRepos(data);
            setLoading(false);
          }
        })
        .catch((e) => {
          if (!signal.aborted) {
            setError(String(e));
            setLoading(false);
          }
        });
    },
    [activeMeshId, load],
  );

  // Recovery actions (restore root to base / free a hostage branch) live
  // in `useMeshRecovery` (issue #283). The hook owns the in-flight flag
  // + the one-line status message + the "always invalidate both caches on
  // success" rule, so the tab can focus on the prune UI. `load` is in
  // `onMutate`'s dep list — when the recovery hook calls it on success,
  // it gets the latest `load` (the version that targets the current
  // mesh).
  const refreshAfterRecovery = useCallback(() => {
    refreshHealth();
    void load();
  }, [refreshHealth, load]);
  const { restore, free, inFlight: recoveryInFlight, message: recoveryMessage } =
    useMeshRecovery(activeMeshId, refreshAfterRecovery);

  // Has this mesh got any health signal to surface? Mirrors the legacy
  // section's `hasHealthSignal` flag so the HealthBlock shows up for the
  // same conditions.
  const hasHealthSignal =
    health !== null &&
    (health.is_drifted ||
      health.base_branch_holder !== null ||
      health.unpushed_ahead > 0 ||
      health.is_dirty);

  const toggle = (key: string) => {
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  // Resolve the current selection into deletion payloads grouped by repo.
  // Memoized on the two inputs that change it: `repos` (the list) and
  // `selected` (the user's checkboxes). Unrelated state changes (e.g.
  // `loading` / `error` / `recoveryMessage`) don't re-walk the list.
  const { branchesByRepo, worktreePaths, branchCount, worktreeCount } = useMemo(() => {
    const byRepo = new Map<string, string[]>();
    const paths: string[] = [];
    for (const repo of repos) {
      for (const b of repo.local_branches) {
        if (selected.has(branchKey(repo.path, b.name))) {
          const list = byRepo.get(repo.path) ?? [];
          list.push(b.name);
          byRepo.set(repo.path, list);
        }
      }
      for (const w of repo.worktrees) {
        if (selected.has(worktreeKey(w.path))) paths.push(w.path);
      }
    }
    const bCount = [...byRepo.values()].reduce((n, l) => n + l.length, 0);
    return { branchesByRepo: byRepo, worktreePaths: paths, branchCount: bCount, worktreeCount: paths.length };
  }, [repos, selected]);
  const selectionEmpty = branchCount === 0 && worktreeCount === 0;

  // Keys for everything we'd recommend pruning across all repos. Memoized
  // on `repos` only — the recommendation rule is a pure function of the
  // prune info.
  const recommendedKeys = useMemo(() => {
    const keys: string[] = [];
    for (const repo of repos) {
      for (const b of repo.local_branches) {
        if (isRecommendedBranch(b)) keys.push(branchKey(repo.path, b.name));
      }
      for (const w of repo.worktrees) {
        if (isRecommendedWorktree(w)) keys.push(worktreeKey(w.path));
      }
    }
    return keys;
  }, [repos]);
  const selectRecommended = () => setSelected(new Set(recommendedKeys));

  const confirmMessage = () => {
    const parts: string[] = [];
    if (branchCount > 0) parts.push(`${branchCount} branch${branchCount === 1 ? '' : 'es'}`);
    if (worktreeCount > 0) parts.push(`${worktreeCount} worktree${worktreeCount === 1 ? '' : 's'}`);
    return `Delete ${parts.join(', ')}? This cannot be undone.`;
  };

  const handleDelete = async () => {
    setConfirming(false);
    setDeleting(true);
    setError(null);
    const errors: string[] = [];
    try {
      for (const [repoPath, names] of branchesByRepo) {
        try {
          await deleteBranches(repoPath, names);
        } catch (e) {
          errors.push(String(e));
        }
      }
      if (worktreePaths.length > 0) {
        try {
          await deleteWorktrees(worktreePaths);
        } catch (e) {
          errors.push(String(e));
        }
      }
    } finally {
      setDeleting(false);
      // Refresh FIRST so load()'s own setError(null) runs, then restore
      // the partial-failure message on top. Pre-fix the message was wiped
      // because load() was fire-and-forget and its synchronous
      // setError(null) immediately clobbered the error we'd just set.
      await load();
      if (errors.length > 0) setError(errors.join('; '));
    }
  };

  // The probe shell's "No project selected" empty state already covers
  // the no-mesh case, so this is belt-and-braces in case the tab is ever
  // mounted standalone.
  if (activeMeshId === null || !activePath) return null;

  return (
    <div className="p-4 space-y-4">
      {hasHealthSignal && health && (
        <HealthBlock
          health={health}
          inFlight={recoveryInFlight}
          onRestore={restore}
          onFree={free}
          message={recoveryMessage}
        />
      )}

      <div className="flex items-center justify-between">
        <div className="flex items-center gap-3">
          <button
            type="button"
            onClick={() => void load()}
            disabled={loading || deleting}
            className="text-xs text-text-muted hover:text-text-primary transition-colors disabled:opacity-50"
          >
            {loading ? 'Loading…' : 'Refresh'}
          </button>
          <button
            type="button"
            onClick={selectRecommended}
            disabled={recommendedKeys.length === 0 || deleting}
            title="Select merged/orphaned clean branches and stale worktrees"
            className="text-xs text-accent-cyan hover:text-accent-cyan/80 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
          >
            Select recommended
            {recommendedKeys.length > 0 ? ` (${recommendedKeys.length})` : ''}
          </button>
        </div>
        <button
          type="button"
          onClick={() => setConfirming(true)}
          disabled={selectionEmpty || deleting}
          className="text-xs text-status-error hover:text-status-error/80 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
        >
          {deleting ? 'Deleting…' : 'Delete Selected'}
        </button>
      </div>

      {error && <p className="text-xs text-status-error break-words">{error}</p>}

      {loading && repos.length === 0 ? (
        <p className="text-xs text-text-muted">Loading git objects…</p>
      ) : (
        <div className="space-y-4">
          {repos.map((repo) => (
            <RepoBlock
              key={repo.path}
              repo={repo}
              selected={selected}
              onToggle={toggle}
              onPruneRemote={async () => {
                try {
                  await pruneRemoteTracking(repo.path);
                  await load();
                } catch (e) {
                  setError(String(e));
                }
              }}
            />
          ))}
        </div>
      )}

      {confirming && (
        <ConfirmDialog
          title="Delete branches & worktrees"
          message={confirmMessage()}
          confirmLabel="Delete"
          onConfirm={handleDelete}
          onCancel={() => setConfirming(false)}
        />
      )}
    </div>
  );
}

// ── Health block (issue #231) ───────────────────────────────────────────────

/**
 * Build a one-line summary of a Mesh's health state. Priority order matches
 * the sidebar badge: hostage first, then drift, then dirty, then unpushed.
 */
function healthOneLiner(health: MeshHealth): string {
  const parts: string[] = [];
  if (health.base_branch_holder) {
    const h = health.base_branch_holder;
    const localBase = health.local_base_branch ?? 'main';
    parts.push(`${localBase} held by ${h.name}`);
  }
  if (health.is_drifted) {
    const localBase = health.local_base_branch ?? 'base';
    const current = health.current_branch ?? `detached @ ${health.current_short_sha}`;
    parts.push(`root on ${current}, base ${localBase}`);
  }
  if (health.is_dirty) parts.push('uncommitted changes');
  if (health.unpushed_ahead > 0) {
    parts.push(
      `${health.unpushed_ahead} unpushed commit${health.unpushed_ahead === 1 ? '' : 's'}`,
    );
  }
  return parts.join(' · ');
}

interface HealthBlockProps {
  health: MeshHealth;
  inFlight: boolean;
  onRestore: () => void;
  onFree: (holder: HoldingWorktree) => void;
  message: string | null;
}

/**
 * The full health card shown at the top of the tab. Surfaces the drift
 * reason(s) and one-click fix buttons whose disabled-state mirrors the
 * backend guard chain (issue #231 — the refuse-rather-than-silently-fail
 * rule). Same shape as the legacy section's HealthBlock; just retuned to
 * the probe's design tokens.
 */
function HealthBlock({ health, inFlight, onRestore, onFree, message }: HealthBlockProps) {
  const localBase = health.local_base_branch ?? 'base';

  // Mirror the backend guard chain for the "Restore root to base" button:
  // disabled when there's a guard that would refuse, with the guard's
  // message in the tooltip so the user knows why.
  const restoreBlockedBy: string | null = (() => {
    if (health.is_dirty) return 'root has uncommitted changes — commit or stash first';
    if (health.unpushed_ahead > 0) {
      const branch = health.current_branch ?? 'HEAD';
      const hint = health.has_upstream ? 'push' : 'push or branch';
      return `${health.unpushed_ahead} unpushed commit(s) on ${branch} — ${hint}, branch, or reset first`;
    }
    if (health.base_branch_holder) {
      return `${localBase} held by ${health.base_branch_holder.name} — free it first`;
    }
    if (!health.is_drifted) {
      return 'already on the base branch';
    }
    return null;
  })();

  return (
    <div className="rounded border border-status-warning/40 bg-status-warning-bg/5 p-2 space-y-2">
      <p className="text-[11px] text-status-warning font-medium">
        {healthOneLiner(health)}
      </p>

      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          onClick={onRestore}
          disabled={inFlight || restoreBlockedBy !== null}
          title={restoreBlockedBy ?? 'Restore the mesh root to the base branch'}
          className="text-xs text-accent-cyan hover:text-accent-cyan/80 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
        >
          Restore root to {localBase}
        </button>

        {health.base_branch_holder && (
          <button
            type="button"
            onClick={() => onFree(health.base_branch_holder!)}
            disabled={inFlight}
            title={
              health.base_branch_holder.is_active
                ? `Detach ${health.base_branch_holder.name}'s HEAD (active agent worktree — safe, non-destructive)`
                : `Detach ${health.base_branch_holder.name}'s HEAD, releasing ${localBase}`
            }
            className="text-xs text-accent-cyan hover:text-accent-cyan/80 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
          >
            Free {localBase} ({health.base_branch_holder.name})
          </button>
        )}
      </div>

      {message && (
        <p className="text-xs text-text-secondary break-words">{message}</p>
      )}
    </div>
  );
}

// ── Repo block (branches / worktrees / remote-tracking) ─────────────────────

interface RepoBlockProps {
  repo: GitRepoPruneInfo;
  selected: Set<string>;
  onToggle: (key: string) => void;
  onPruneRemote: () => void | Promise<void>;
}

/**
 * One mesh-included repo's worth of prune info. Each repo gets its own
 * local-branches + worktrees + remote-tracking section. The path is shown
 * as a one-line header so a mesh with multiple nested repos stays
 * readable.
 */
function RepoBlock({ repo, selected, onToggle, onPruneRemote }: RepoBlockProps) {
  return (
    <div className="space-y-3 rounded border border-border-subtle p-3">
      <p className="text-[10px] font-mono text-text-muted truncate" title={repo.path}>
        {repo.path}
      </p>

      {/* Local branches */}
      <div>
        <p className="text-[10px] uppercase tracking-wide text-text-muted mb-1">
          Local branches
        </p>
        {repo.local_branches.length === 0 ? (
          <p className="text-xs text-text-muted">None</p>
        ) : (
          <div className="space-y-0.5">
            {repo.local_branches.map((b) => {
              const key = branchKey(repo.path, b.name);
              return (
                <label
                  key={key}
                  className="flex items-center gap-2 text-xs cursor-pointer hover:bg-bg-overlay/40 rounded px-1 py-0.5"
                >
                  <input
                    type="checkbox"
                    checked={selected.has(key)}
                    onChange={() => onToggle(key)}
                    className="accent-accent-cyan"
                  />
                  <span className="text-text-primary truncate flex-1">{b.name}</span>
                  <span className="flex items-center gap-1 flex-shrink-0">
                    {b.is_head && (
                      <Badge color="bg-accent-cyan/15 text-accent-cyan" text="HEAD" />
                    )}
                    {b.is_merged_into_main && (
                      <Badge color="bg-status-success/15 text-status-success" text="merged" />
                    )}
                    {b.is_orphan && (
                      <Badge color="bg-status-warning/15 text-status-warning" text="orphan" />
                    )}
                    {!b.has_uncommitted && (
                      <Badge color="bg-bg-overlay text-text-muted" text="clean" />
                    )}
                    {(b.ahead > 0 || b.behind > 0) && (
                      <span className="text-[9px] font-mono text-text-muted">
                        ↑{b.ahead} ↓{b.behind}
                      </span>
                    )}
                    {b.last_commit_date && (
                      <span className="text-[9px] text-text-muted">
                        {formatDate(b.last_commit_date)}
                      </span>
                    )}
                  </span>
                </label>
              );
            })}
          </div>
        )}
      </div>

      {/* Worktrees */}
      <div>
        <p className="text-[10px] uppercase tracking-wide text-text-muted mb-1">
          Worktrees
        </p>
        {repo.worktrees.length === 0 ? (
          <p className="text-xs text-text-muted">None</p>
        ) : (
          <div className="space-y-0.5">
            {repo.worktrees.map((w) => {
              const key = worktreeKey(w.path);
              const name = w.path.split(/[/\\]/).pop() || w.path;
              return (
                <label
                  key={key}
                  title={w.is_active ? 'Active — cannot delete' : w.path}
                  className={`flex items-center gap-2 text-xs rounded px-1 py-0.5 ${
                    w.is_active
                      ? 'opacity-60 cursor-not-allowed'
                      : 'cursor-pointer hover:bg-bg-overlay/40'
                  }`}
                >
                  <input
                    type="checkbox"
                    checked={selected.has(key)}
                    disabled={w.is_active}
                    onChange={() => onToggle(key)}
                    className="accent-accent-cyan disabled:cursor-not-allowed"
                  />
                  <span className="text-text-primary truncate flex-1">
                    {name}
                    {w.branch && (
                      <span className="text-text-muted"> · {w.branch}</span>
                    )}
                  </span>
                  <span className="flex items-center gap-1 flex-shrink-0">
                    {w.is_active && (
                      <Badge
                        color="bg-accent-cyan/15 text-accent-cyan"
                        text="active"
                        title="Active — cannot delete"
                      />
                    )}
                    {w.is_stale && (
                      <Badge color="bg-status-warning/15 text-status-warning" text="stale" />
                    )}
                  </span>
                </label>
              );
            })}
          </div>
        )}
      </div>

      {/* Remote-tracking branches */}
      {repo.remote_tracking_branches.length > 0 && (
        <div>
          <div className="flex items-center justify-between mb-1">
            <p className="text-[10px] uppercase tracking-wide text-text-muted">
              Remote-tracking
            </p>
            <button
              type="button"
              onClick={() => void onPruneRemote()}
              className="text-[10px] text-text-muted hover:text-text-primary transition-colors"
            >
              Prune
            </button>
          </div>
          <div className="space-y-0.5">
            {repo.remote_tracking_branches.map((name) => (
              <div
                key={name}
                className="text-[10px] font-mono text-text-muted px-1 truncate"
              >
                {name}
              </div>
            ))}
          </div>
        </div>
      )}
    </div>
  );
}
