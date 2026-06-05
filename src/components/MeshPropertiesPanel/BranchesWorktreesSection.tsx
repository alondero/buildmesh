import { useCallback, useEffect, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
import { useMeshHealth } from '../../hooks/useMeshHealth';
import {
  restoreMeshToBase,
  freeBaseBranch,
  type MeshHealth,
  type HoldingWorktree,
} from '../../lib/tauri';

interface BranchInfo {
  name: string;
  is_head: boolean;
  is_merged_into_main: boolean | null;
  is_orphan: boolean;
  has_uncommitted: boolean;
  last_commit_date: string | null;
  ahead: number;
  behind: number;
}

interface WorktreeInfo {
  path: string;
  branch: string | null;
  is_active: boolean;
  is_stale: boolean;
}

interface GitRepoPruneInfo {
  path: string;
  local_branches: BranchInfo[];
  worktrees: WorktreeInfo[];
  remote_tracking_branches: string[];
}

interface Props {
  meshId: number;
  /** Absolute host path of the Mesh root. Required so the `useMeshHealth`
   * hook can install its `git-changed` listener and auto-refresh when a
   * background `git checkout` (e.g. from a terminal) changes the root's
   * HEAD. Without it the panel only refreshes from explicit user actions
   * (Restore / Free button clicks). */
  meshPath: string;
}

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

// Composite keys keep selection unambiguous across multiple repos.
const branchKey = (repo: string, name: string) => `b:${repo}::${name}`;
const worktreeKey = (path: string) => `w:${path}`;

// "Safe to prune" recommendation. A branch is recommended when it isn't the
// current HEAD, has nothing uncommitted to lose, and is either fully merged
// into main or orphaned (its upstream remote branch is gone). A worktree is
// recommended when its branch no longer exists (stale) and no agent is using it.
const isRecommendedBranch = (b: BranchInfo) =>
  !b.is_head && !b.has_uncommitted && (b.is_merged_into_main === true || b.is_orphan);

const isRecommendedWorktree = (w: WorktreeInfo) => !w.is_active && w.is_stale;

export function BranchesWorktreesSection({ meshId, meshPath }: Props) {
  const [collapsed, setCollapsed] = useState(true);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [repos, setRepos] = useState<GitRepoPruneInfo[]>([]);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [confirming, setConfirming] = useState(false);
  const [deleting, setDeleting] = useState(false);
  // Health recovery: track in-flight actions and the last result so the
  // UI can disable buttons during the call and show a one-line status.
  const [restoreInFlight, setRestoreInFlight] = useState(false);
  const [freeInFlight, setFreeInFlight] = useState(false);
  const [recoveryMessage, setRecoveryMessage] = useState<string | null>(null);
  // The hook is keyed on `meshId`; meshPath lets the GIT_CHANGED listener
  // fire on background `git checkout` events from outside the app, so the
  // health block stays in sync with the root's actual HEAD without a
  // window-focus round-trip.
  const { health, refresh: refreshHealth } = useMeshHealth(meshId, meshPath);

  const load = useCallback(() => {
    setLoading(true);
    setError(null);
    invoke<GitRepoPruneInfo[]>('get_git_prune_info', { meshId })
      .then((data) => {
        setRepos(data);
        setSelected(new Set());
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, [meshId]);

  // Lazy-load the first time the section is expanded.
  useEffect(() => {
    if (!collapsed && repos.length === 0 && !loading && !error) {
      load();
    }
  }, [collapsed, repos.length, loading, error, load]);

  const handleRestore = async () => {
    setRestoreInFlight(true);
    setRecoveryMessage(null);
    try {
      const result = await restoreMeshToBase(meshId);
      setRecoveryMessage(result.message);
      refreshHealth();
      load();
    } catch (e) {
      setRecoveryMessage(`Restore error: ${e}`);
    } finally {
      setRestoreInFlight(false);
    }
  };

  const handleFree = async (holder: HoldingWorktree) => {
    setFreeInFlight(true);
    setRecoveryMessage(null);
    try {
      const result = await freeBaseBranch(meshId, holder.path);
      setRecoveryMessage(`Freed base branch at ${result.detached_at_sha}`);
      refreshHealth();
      load();
    } catch (e) {
      setRecoveryMessage(`Free error: ${e}`);
    } finally {
      setFreeInFlight(false);
    }
  };

  // Has this mesh got any health signal to surface?
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
  const branchesByRepo = new Map<string, string[]>();
  const worktreePaths: string[] = [];
  for (const repo of repos) {
    for (const b of repo.local_branches) {
      if (selected.has(branchKey(repo.path, b.name))) {
        const list = branchesByRepo.get(repo.path) ?? [];
        list.push(b.name);
        branchesByRepo.set(repo.path, list);
      }
    }
    for (const w of repo.worktrees) {
      if (selected.has(worktreeKey(w.path))) worktreePaths.push(w.path);
    }
  }
  const branchCount = [...branchesByRepo.values()].reduce((n, l) => n + l.length, 0);
  const worktreeCount = worktreePaths.length;
  const selectionEmpty = branchCount === 0 && worktreeCount === 0;

  // Keys for everything we'd recommend pruning across all repos.
  const recommendedKeys: string[] = [];
  for (const repo of repos) {
    for (const b of repo.local_branches) {
      if (isRecommendedBranch(b)) recommendedKeys.push(branchKey(repo.path, b.name));
    }
    for (const w of repo.worktrees) {
      if (isRecommendedWorktree(w)) recommendedKeys.push(worktreeKey(w.path));
    }
  }
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
          await invoke('delete_branches', { worktreePath: repoPath, branchNames: names });
        } catch (e) {
          errors.push(String(e));
        }
      }
      if (worktreePaths.length > 0) {
        try {
          await invoke('delete_worktrees', { worktreePaths });
        } catch (e) {
          errors.push(String(e));
        }
      }
    } finally {
      setDeleting(false);
      if (errors.length > 0) setError(errors.join('; '));
      load();
    }
  };

  return (
    <div className="rounded border border-[#2a2a2a]">
      <button
        onClick={() => setCollapsed((c) => !c)}
        className="w-full flex items-center justify-between px-3 py-2 text-xs font-medium text-[#e0e0e0] hover:bg-[#1a1a2e]/40 transition-colors"
      >
        <span className="flex items-center gap-2">
          Branches &amp; Worktrees
          {hasHealthSignal && (
            <span
              className="text-[10px] font-bold text-status-warning bg-status-warning-bg/15 rounded px-1.5 leading-[16px]"
              title={health ? healthOneLiner(health) : ''}
            >
              !
            </span>
          )}
        </span>
        <svg
          width="12"
          height="12"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="2"
          strokeLinecap="round"
          strokeLinejoin="round"
          className={`transition-transform ${collapsed ? '' : 'rotate-90'}`}
        >
          <polyline points="9 18 15 12 9 6" />
        </svg>
      </button>

      {/* Always-visible one-liner when the section is collapsed and the
          mesh has a health signal — surfaces the problem without forcing
          the user to expand. */}
      {collapsed && hasHealthSignal && health && (
        <p
          className="px-3 pb-2 text-[10px] text-status-warning truncate"
          title={healthOneLiner(health)}
        >
          {healthOneLiner(health)}
        </p>
      )}

      {!collapsed && (
        <div className="px-3 pb-3 space-y-3 border-t border-[#2a2a2a] pt-3">
          {hasHealthSignal && health && (
            <HealthBlock
              health={health}
              inFlight={restoreInFlight || freeInFlight}
              onRestore={handleRestore}
              onFree={handleFree}
              message={recoveryMessage}
            />
          )}
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-3">
              <button
                onClick={load}
                disabled={loading || deleting}
                className="text-[10px] text-[#9ca3af] hover:text-[#e0e0e0] transition-colors disabled:opacity-50"
              >
                {loading ? 'Loading…' : 'Refresh'}
              </button>
              <button
                onClick={selectRecommended}
                disabled={recommendedKeys.length === 0 || deleting}
                title="Select merged/orphaned clean branches and stale worktrees"
                className="text-[10px] text-[#00d4ff] hover:text-[#7fe5ff] transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
              >
                Select recommended{recommendedKeys.length > 0 ? ` (${recommendedKeys.length})` : ''}
              </button>
            </div>
            <button
              onClick={() => setConfirming(true)}
              disabled={selectionEmpty || deleting}
              className="text-[10px] text-[#ef4444] hover:text-[#ff6b6b] transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
            >
              {deleting ? 'Deleting…' : 'Delete Selected'}
            </button>
          </div>

          {error && <p className="text-[10px] text-red-400 break-words">{error}</p>}

          {loading && repos.length === 0 ? (
            <p className="text-[10px] text-[#6b7280]">Loading git objects…</p>
          ) : (
            repos.map((repo) => (
              <div key={repo.path} className="space-y-2">
                {/* Local branches */}
                <div>
                  <p className="text-[10px] uppercase tracking-wide text-[#6b7280] mb-1">
                    Local branches
                  </p>
                  {repo.local_branches.length === 0 ? (
                    <p className="text-[10px] text-[#6b7280]">None</p>
                  ) : (
                    <div className="space-y-0.5">
                      {repo.local_branches.map((b) => {
                        const key = branchKey(repo.path, b.name);
                        return (
                          <label
                            key={key}
                            className="flex items-center gap-2 text-xs cursor-pointer hover:bg-[#1a1a2e]/40 rounded px-1 py-0.5"
                          >
                            <input
                              type="checkbox"
                              checked={selected.has(key)}
                              onChange={() => toggle(key)}
                              className="accent-[#00d4ff]"
                            />
                            <span className="text-[#d1d5db] truncate flex-1">{b.name}</span>
                            <span className="flex items-center gap-1 flex-shrink-0">
                              {b.is_head && (
                                <Badge color="bg-[#00d4ff]/15 text-[#00d4ff]" text="HEAD" />
                              )}
                              {b.is_merged_into_main && (
                                <Badge color="bg-green-400/15 text-green-400" text="merged" />
                              )}
                              {b.is_orphan && (
                                <Badge color="bg-amber-400/15 text-amber-400" text="orphan" />
                              )}
                              {!b.has_uncommitted && (
                                <Badge color="bg-[#2a2a2a] text-[#9ca3af]" text="clean" />
                              )}
                              {(b.ahead > 0 || b.behind > 0) && (
                                <span className="text-[9px] font-mono text-[#6b7280]">
                                  ↑{b.ahead} ↓{b.behind}
                                </span>
                              )}
                              {b.last_commit_date && (
                                <span className="text-[9px] text-[#6b7280]">
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
                  <p className="text-[10px] uppercase tracking-wide text-[#6b7280] mb-1">
                    Worktrees
                  </p>
                  {repo.worktrees.length === 0 ? (
                    <p className="text-[10px] text-[#6b7280]">None</p>
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
                                : 'cursor-pointer hover:bg-[#1a1a2e]/40'
                            }`}
                          >
                            <input
                              type="checkbox"
                              checked={selected.has(key)}
                              disabled={w.is_active}
                              onChange={() => toggle(key)}
                              className="accent-[#00d4ff] disabled:cursor-not-allowed"
                            />
                            <span className="text-[#d1d5db] truncate flex-1">
                              {name}
                              {w.branch && (
                                <span className="text-[#6b7280]"> · {w.branch}</span>
                              )}
                            </span>
                            <span className="flex items-center gap-1 flex-shrink-0">
                              {w.is_active && (
                                <Badge
                                  color="bg-[#00d4ff]/15 text-[#00d4ff]"
                                  text="active"
                                  title="Active — cannot delete"
                                />
                              )}
                              {w.is_stale && (
                                <Badge color="bg-amber-400/15 text-amber-400" text="stale" />
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
                      <p className="text-[10px] uppercase tracking-wide text-[#6b7280]">
                        Remote-tracking
                      </p>
                      <button
                        onClick={() =>
                          invoke('prune_remote_tracking', { worktreePath: repo.path })
                            .then(load)
                            .catch((e) => setError(String(e)))
                        }
                        className="text-[9px] text-[#9ca3af] hover:text-[#e0e0e0] transition-colors"
                      >
                        Prune
                      </button>
                    </div>
                    <div className="space-y-0.5">
                      {repo.remote_tracking_branches.map((name) => (
                        <div key={name} className="text-[10px] font-mono text-[#6b7280] px-1 truncate">
                          {name}
                        </div>
                      ))}
                    </div>
                  </div>
                )}
              </div>
            ))
          )}
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
 * Build a one-line summary of a Mesh's health state. Used in both the
 * section header (when collapsed) and the inline tooltip. Priority order
 * matches the sidebar badge: hostage first, then drift, then dirty, then
 * unpushed.
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
    parts.push(`${health.unpushed_ahead} unpushed commit${health.unpushed_ahead === 1 ? '' : 's'}`);
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
 * The full health card shown at the top of the expanded
 * `BranchesWorktreesSection` body. Surfaces the drift reason(s) and
 * one-click fix buttons whose disabled-state mirrors the backend guard
 * chain (issue #231 — the refuse-rather-than-silently-fail rule).
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
      // Not drifted AND no upstream unpushed and no hostage — nothing to do.
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
          className="text-[10px] text-[#00d4ff] hover:text-[#7fe5ff] transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
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
            className="text-[10px] text-[#00d4ff] hover:text-[#7fe5ff] transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
          >
            Free {localBase} ({health.base_branch_holder.name})
          </button>
        )}
      </div>

      {message && (
        <p className="text-[10px] text-text-secondary break-words">{message}</p>
      )}
    </div>
  );
}
