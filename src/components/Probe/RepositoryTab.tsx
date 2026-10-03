/**
 * RepositoryTab — the Probe Panel's Repository destination (issue #1460).
 *
 * This destination is **maintenance only**. It used to be the "Worktree
 * Manager" and carried two unrelated jobs in one column: a worktree
 * configuration card (use-worktree, base ref, mode, warm pool, worktree
 * directory) and the Git-maintenance surface. Issue #1460 split them;
 * configuration now lives in `ProjectSettingsTab`, and what remains here is
 * the operational half:
 *
 *   1. **Health and recovery** — drift detection, base-branch hostage, and
 *      the one-click Restore / Free buttons. The shared `useMeshHealth` and
 *      `useMeshRecovery` hooks are the source of truth (the sidebar's `!`
 *      badge reads from the same hook, so the two cannot disagree about the
 *      project's state).
 *   2. **Branches and worktrees** — list local branches and worktrees per
 *      repo (a project can include nested repos), with a "recommended"
 *      selection helper for merged + orphan + clean branches and stale
 *      worktrees. Delete goes through a `ConfirmDialog`.
 *   3. **Remote-tracking prune** — per-repo button to drop local refs to
 *      remote branches whose upstream is gone.
 *
 * The two jobs sit in separate `ProbeSection` frames because the issue's
 * acceptance criteria require recovery to read as different from cleanup:
 * "Restore root to main" and "Delete 4 branches" are not the same class of
 * click, and one undifferentiated heading is exactly what made the old
 * surface hard to trust. Nothing here writes configuration — the
 * `updateMesh*` wrappers are gone from the imports for that reason, and a
 * regression test asserts the strategy controls are absent.
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
 *
 * The destination id is still `worktrees` and is deliberately NOT renamed:
 * ADR-0030 keeps `probe-<tab>` ids stable for existing callers and deep
 * links. See `docs/adr/0038-project-settings-vs-repository.md`.
 */

import { formatError } from '../../lib/errorUtils';
import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useMeshHealth } from '../../hooks/useMeshHealth';
import { useMeshRecovery } from '../../hooks/useMeshRecovery';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
import {
  deleteBranches,
  deleteWorktrees,
  getGitPruneInfo,
  openInFileManager,
  pruneRemoteTracking,
  type BranchInfo,
  type GitRepoPruneInfo,
  type HoldingWorktree,
  type MeshHealth,
  type WorktreeInfo,
} from '../../lib/tauri';
import { LoadingState, RefreshControl } from '../shared/Spinner';
import { ProbeTabBody } from './ProbeTabBody';
import { ProbeScopeNote, ProbeSection } from './ProbeSection';
function FolderOpenIcon({ className }: { className?: string }) {
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
      <path d="M6 14l1.45-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.55 6a2 2 0 0 1-1.94 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.93a2 2 0 0 1 1.66.9l.82 1.2a2 2 0 0 0 1.66.9H18a2 2 0 0 1 2 2v2" />
    </svg>
  );
}

/**
 * Open a path in the OS file manager. Centralised so the `RepoBlock`
 * repo-path header and the per-worktree rows use the same try/catch —
 * `open_in_file_manager` rejects when the path doesn't exist or isn't
 * a directory (common for stale worktree rows after a `git worktree
 * prune`), and we don't want one bad row to spam the console with a
 * red error on every render.
 */
const openInExplorer = async (path: string) => {
  try {
    await openInFileManager(path);
  } catch (e) {
    console.error('Failed to open folder in file manager:', e);
  }
};

const Badge = ({ color, text, title }: { color: string; text: string; title?: string }) => (
  <span
    title={title}
    className={`px-1 py-px rounded text-2xs font-medium leading-none ${color}`} /* allow-bare-rounded */
  >
    {/* 2xs status badge — intentionally smallest radius, no interaction */}
    {text}
  </span>
);

function formatDate(iso: string | null): string {
  if (!iso) return '';
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return '';
  return d.toLocaleDateString(undefined, { year: 'numeric', month: 'short', day: 'numeric' });
}

// Last path segment — used to render a short, friendly worktree or
// branch-row label from a full path (e.g. `/repos/foo/.claude/worktrees/abc`
// → `abc`). Falls back to the original string when the path has no
// separator (root paths, edge cases) so the UI never shows "undefined".
const pathDirname = (p: string): string => p.split(/[/\\]/).pop() ?? p;

// Composite keys keep selection unambiguous across multiple repos (a Mesh
// can include nested git repos — each contributes its own branch list).
const branchKey = (repo: string, name: string) => `b:${repo}::${name}`;
const worktreeKey = (path: string) => `w:${path}`;

// "Safe to prune" recommendation. A branch is recommended when it isn't the
// current HEAD, has nothing uncommitted to lose, isn't held by an active
// agent node, isn't checked out as the HEAD of *any* working tree on disk
// (main or linked, live or orphan), and is either fully merged into main or
// orphaned (its upstream remote branch is gone). The `!is_active` clause
// mirrors the worktree rule — a live agent on a branch must close the node
// first before the branch becomes prunable. The `!checked_out_in_worktree`
// clause catches the orphan-worktree case `is_active` misses: when an
// agent node was deleted/archived but its directory survives, the branch
// is HEAD of a working tree git refuses to delete, and the only safe path
// is to remove the worktree (which cascades to the branch via
// `remove_one_worktree_and_branch`). A worktree is recommended when its
// branch no longer exists (stale) and no agent is using it. Mirrors the
// legacy section's logic.
const isRecommendedBranch = (b: BranchInfo) =>
  !b.is_head &&
  !b.has_uncommitted &&
  !b.is_active &&
  !b.checked_out_in_worktree &&
  (b.is_merged_into_main === true || b.is_orphan);

const isRecommendedWorktree = (w: WorktreeInfo) => !w.is_active && w.is_stale;

export function RepositoryTab() {
  const { activeMeshId } = useProbeContext();
  // Cleanup selections and confirmations belong to one mesh. Remounting the
  // owner removes those targets synchronously when the probe changes scope.
  return <RepositoryContents key={activeMeshId ?? 'none'} />;
}

function RepositoryContents() {
  // This is a project-scoped destination: the health snapshot and the
  // Restore/Free recovery actions walk the project ROOT, not a focused
  // agent's worktree. Use `activeMeshPath` (the mesh row's own path) —
  // `activePath` resolves to the focused node's worktree subdir when a node
  // is active, which would key the health file-watch subscription off the
  // wrong directory and diverge from the sidebar `!` badge (which uses the
  // mesh root). See the `useProbeContext` contract and `ProjectSettingsTab`
  // (issue #809). The body says so out loud via `<ProbeScopeNote>` because
  // issue #1460 requires mesh-root vs active-node semantics to be explicit
  // rather than inferred from the header.
  const { activeMeshId, activeMeshPath, activeMeshName } = useProbeContext();

  // The health hook subscribes to the shared cache (used by the sidebar's
  // `!` badge too) and refetches on GIT_CHANGED / focus. The prune info is
  // local to this tab — only it cares about the full list, so it doesn't
  // share a cache.
  const { health, refresh: refreshHealth } = useMeshHealth(activeMeshId, activeMeshPath);

  const [repos, setRepos] = useState<GitRepoPruneInfo[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [selected, setSelected] = useState<Set<string>>(new Set());
  const [confirming, setConfirming] = useState(false);
  const [deleting, setDeleting] = useState(false);
  // Issue #657 — per-repo in-flight flag for `prune_remote_tracking`, plus
  // a transient success message that auto-clears after 4s (matches the
  // `gitSync` pattern in `src/components/Sidebar/MeshItem.tsx`). The
  // existing `error` channel is reserved for prune / recovery / remote-
  // tracking failures — we prefix prune failures with "Prune failed: "
  // so the user can distinguish them from `deleteBranches`/`deleteWorktrees`
  // errors that share the same renderer.
  const [pruningPaths, setPruningPaths] = useState<Set<string>>(new Set());
  const [successMessage, setSuccessMessage] = useState<string | null>(null);
  const successTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const mountedRef = useRef(true);
  const loadRevisionRef = useRef(0);

  // Single prune-fetch body. Mount, mesh-switch, manual Refresh, and
  // post-recovery all route through `load`. The function returns the
  // promise so callers that need to sequence AFTER the refresh (e.g.
  // `handleDelete` wants the partial-failure error to land on top of
  // `load`'s own `setError(null)`) can `await load()`. A shared revision
  // gates mount, manual and post-mutation refreshes,
  // so a rapid Refresh click while the previous fetch is still pending
  // can't clobber state with a stale response.
  const load = useCallback(() => {
    if (activeMeshId === null || !mountedRef.current) return Promise.resolve();
    const revision = ++loadRevisionRef.current;
    const current = () => mountedRef.current && revision === loadRevisionRef.current;
    setLoading(true);
    setError(null);
    setSelected(new Set());
    return getGitPruneInfo(activeMeshId)
      .then((data) => {
        if (current()) setRepos(data);
      })
      .catch((e) => { if (current()) setError(formatError(e)); })
      .finally(() => { if (current()) setLoading(false); });
  }, [activeMeshId]);

  // Mesh-switch reset (review findings B1 + B2): clear any in-flight
  // pruning set (the repo paths belong to the previous mesh) and wipe
  // the transient success message + its 4s timer. Lives in a separate
  // effect (NOT inside `load`) so a successful prune's own `load()` call
  // doesn't wipe the success message we just set.
  useEffect(() => {
    setPruningPaths(new Set());
    setSuccessMessage(null);
    if (successTimerRef.current) {
      clearTimeout(successTimerRef.current);
      successTimerRef.current = null;
    }
  }, [activeMeshId]);

  useEffect(() => {
    const revisions = loadRevisionRef;
    mountedRef.current = true;
    void load();
    return () => {
      mountedRef.current = false;
      ++revisions.current;
    };
  }, [load]);

  // Issue #657 — clear any pending prune-success timer on unmount so a
  // late-firing `setSuccessMessage` can't land on an unmounted tree
  // (e.g. user switches meshes or closes the probe before the 4s
  // auto-clear fires).
  useEffect(() => {
    return () => {
      if (successTimerRef.current) {
        clearTimeout(successTimerRef.current);
        successTimerRef.current = null;
      }
    };
  }, []);

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
    if (loading || deleting) return;
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

  // Issue #1460: the confirmation states the scope (which project, how many
  // branches, how many worktrees) AND the recovery story. "This cannot be
  // undone" alone left the user guessing whether the project's own files
  // were involved; they are not — only the refs and the linked working
  // directories go.
  const confirmMessage = () => {
    const parts: string[] = [];
    if (branchCount > 0) parts.push(`${branchCount} branch${branchCount === 1 ? '' : 'es'}`);
    if (worktreeCount > 0) parts.push(`${worktreeCount} worktree${worktreeCount === 1 ? '' : 's'}`);
    const subject = activeMeshName ? ` from "${activeMeshName}"` : '';
    return `Delete ${parts.join(' and ')}${subject}? Uncommitted changes in those worktrees are lost, and deleted branches can only be recovered through git reflog if it has not expired. This cannot be undone.`;
  };

  const handleDelete = async () => {
    // `activeMeshId` is the project-scoped key `deleteBranches` needs to
    // scope the active-branch guard. The tab early-returns at the
    // bottom when no mesh is active, but TypeScript can't narrow that
    // across the closure boundary here, so guard locally. We close the
    // dialog + surface an error rather than silently freezing the modal
    // — a stale closure (mesh switched after the dialog opened) would
    // otherwise leave the user staring at a non-responsive confirmation.
    if (activeMeshId === null) {
      setConfirming(false);
      setError('No active project — cannot delete.');
      return;
    }
    setConfirming(false);
    setDeleting(true);
    setError(null);
    const errors: string[] = [];
    try {
      for (const [repoPath, names] of branchesByRepo) {
        try {
          await deleteBranches(activeMeshId, repoPath, names);
        } catch (e) {
          errors.push(formatError(e));
        }
      }
      if (worktreePaths.length > 0) {
        try {
          await deleteWorktrees(worktreePaths);
        } catch (e) {
          errors.push(formatError(e));
        }
      }
    } finally {
      if (mountedRef.current) {
        setDeleting(false);
        // Refresh first, then restore partial deletion failures after load()
        // has cleared the previous error.
        await load();
        if (mountedRef.current && errors.length > 0) setError(errors.join('; '));
      }
    }
  };

  // The probe shell's "No project selected" empty state already covers
  // the no-mesh case, so this is belt-and-braces in case the tab is ever
  // mounted standalone.
  if (activeMeshId === null || !activeMeshPath) return null;

  return (
    <ProbeTabBody padding="p-3" className="space-y-4">
      {/* Section 1 — health and recovery. Always rendered (issue #1460:
          the old tab rendered the health card ONLY when something was
          wrong, so "no health section" was indistinguishable from "health
          not checked yet"). An explicit healthy line is what makes recovery
          legible as its own job rather than an error banner that happens to
          be absent. */}
      <ProbeSection
        title="Health and recovery"
        testId="repository-health-section"
        description="Drift, a held base branch, and unpushed commits in the project root."
      >
        <ProbeScopeNote>
          Acts on the project root <code className="break-all">{activeMeshPath}</code>, not the
          focused agent&apos;s worktree.
        </ProbeScopeNote>
        {health === null ? (
          <LoadingState label="Checking repository health…" />
        ) : hasHealthSignal ? (
          <HealthBlock
            health={health}
            inFlight={recoveryInFlight}
            onRestore={restore}
            onFree={free}
            message={recoveryMessage}
          />
        ) : (
          <p className="text-xs text-text-muted" data-testid="repository-healthy">
            No drift, no held base branch, and nothing unpushed.
          </p>
        )}
      </ProbeSection>

      {/* Section 2 — cleanup. Deliberately a different frame from recovery:
          the buttons here delete, the buttons above repair. */}
      <ProbeSection
        title="Branches and worktrees"
        testId="repository-cleanup-section"
        description="Delete merged or orphaned branches and stale worktrees, and prune remote-tracking refs."
      >
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            {/* `variant="muted"` keeps the refresh visually quieter than
                the cyan Select-recommended / red Delete-Selected
                neighbours — issue #842. The shared `<RefreshControl>`
                picks up the spinner/busy state for free. */}
            <RefreshControl
              onRefresh={() => void load()}
              isRefreshing={loading}
              disabled={deleting}
              variant="muted"
              ariaLabel="Refresh branches and worktrees"
            />
            <button
              type="button"
              onClick={selectRecommended}
              disabled={recommendedKeys.length === 0 || deleting || loading}
              title="Select merged/orphaned clean branches and stale worktrees"
              className="text-xs text-text-secondary hover:text-text-primary transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
            >
              Select recommended
              {recommendedKeys.length > 0 ? ` (${recommendedKeys.length})` : ''}
            </button>
          </div>
          <button
            type="button"
            onClick={() => setConfirming(true)}
            disabled={selectionEmpty || deleting || loading}
            className="text-xs text-status-error hover:text-status-error/80 transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
          >
            {deleting ? 'Deleting…' : 'Delete Selected'}
          </button>
        </div>

        {error && <p className="text-xs text-status-error break-words">{error}</p>}
        {/* Issue #657 — success channel for prune_remote_tracking. Same
            inline style + 4s auto-clear as the `gitSync` pattern in
            `MeshItem.tsx` (cleaner than wiring the global toast system
            for one button). Styled `text-status-success` to mirror the
            red error above. */}
        {successMessage && (
          <p className="text-xs text-status-success whitespace-pre-wrap break-words">{successMessage}</p>
        )}

        {loading && repos.length === 0 ? (
          <LoadingState label="Loading git objects…" />
        ) : (
          <div className="space-y-4">
            {repos.map((repo) => (
              <RepoBlock
                key={repo.path}
                repo={repo}
                selected={selected}
                onToggle={toggle}
                selectionDisabled={loading || deleting}
                pruning={pruningPaths.has(repo.path)}
                onPruneRemote={async () => {
                  // Mark this repo as pruning so the button shows
                  // "Pruning…" + disabled until the invoke resolves
                  // (issue #657, AC2).
                  setPruningPaths((prev) => {
                    const next = new Set(prev);
                    next.add(repo.path);
                    return next;
                  });
                  // Mirrors the legacy `handleDelete` pattern — accumulate
                  // partial-failure errors, refresh in `finally`, then
                  // restore the error AFTER `load()`'s own
                  // `setError(null)` runs. Keeps a post-prune refresh
                  // failure from being mislabelled "Prune failed:"
                  // (review finding A1/C1).
                  let pruneError: string | null = null;
                  try {
                    const message = await pruneRemoteTracking(repo.path);
                    if (!mountedRef.current) return;
                    // git returns its (possibly empty) report on stderr.
                    // The Rust side already trims; empty stderr means
                    // "nothing was pruned" — surface a fallback so the
                    // user always sees something.
                    const display = message || 'Remote-tracking refs pruned.';
                    setSuccessMessage(display);
                    if (successTimerRef.current) clearTimeout(successTimerRef.current);
                    successTimerRef.current = setTimeout(() => setSuccessMessage(null), 4000);
                  } catch (e) {
                    if (!mountedRef.current) return;
                    // Prefix distinguishes prune failures from
                    // `deleteBranches`/`deleteWorktrees` failures that
                    // share the same inline-error renderer (AC4). Also
                    // clear any stale success message — the two channels
                    // are mutually exclusive (review finding A2/B3).
                    setSuccessMessage(null);
                    if (successTimerRef.current) {
                      clearTimeout(successTimerRef.current);
                      successTimerRef.current = null;
                    }
                    pruneError = `Prune failed: ${formatError(e)}`;
                  } finally {
                    if (mountedRef.current) {
                      setPruningPaths((prev) => {
                        const next = new Set(prev);
                        next.delete(repo.path);
                        return next;
                      });
                      // Refresh failures should not be labelled as prune failures.
                      await load();
                      // Restore the prune error after load() clears the old error.
                      if (mountedRef.current && pruneError) setError(pruneError);
                    }
                  }
                }}
              />
            ))}
          </div>
        )}
      </ProbeSection>

      {confirming && (
        <ConfirmDialog
          title="Delete branches & worktrees"
          message={confirmMessage()}
          confirmLabel="Delete"
          onConfirm={handleDelete}
          onCancel={() => setConfirming(false)}
        />
      )}
    </ProbeTabBody>
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
      return 'already on the Base Ref';
    }
    return null;
  })();

  return (
    <div className="rounded-md border border-status-warning/40 bg-status-warning/5 p-2 space-y-2">
      <p className="text-xs text-status-warning font-medium">
        {healthOneLiner(health)}
      </p>

      <div className="flex flex-wrap items-center gap-2">
        <button
          type="button"
          onClick={onRestore}
          disabled={inFlight || restoreBlockedBy !== null}
          title={restoreBlockedBy ?? 'Restore the mesh root to the Base Ref'}
          className="text-xs text-text-secondary hover:text-text-primary transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
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
            className="text-xs text-text-secondary hover:text-text-primary transition-colors disabled:opacity-40 disabled:cursor-not-allowed"
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

// ── Selectable row (issue #661) ────────────────────────────────────────────

interface SelectableRowProps {
  /** Unique key for selection lookup; used as the React `key` at the call
   *  site (the component takes no `key` prop — that's an anti-pattern). */
  rowKey: string;
  /** Hover tooltip on the row (e.g. "Active — cannot delete"). */
  title?: string;
  /** Whether the checkbox is disabled AND the row is dimmed. The two
   *  always co-vary — a disabled checkbox is useless if the row still
   *  looks interactive, and an undeletable-but-bright row confuses the
   *  user about what's lockable. Branch rows: disabled when active or
   *  checked out as a worktree head; worktree rows: disabled when
   *  active or pool-managed. */
  disabled: boolean;
  /** Whether the row is currently checked in the parent's selection set. */
  selected: boolean;
  /** Toggle callback fired with `rowKey` so the parent can mutate its Set. */
  onToggle: (key: string) => void;
  /** Primary text (left of badges). Plain string for a single name;
   *  JSX for branch-name + secondary branch-tag (worktree rows). */
  children: React.ReactNode;
  /** Right-aligned status badges. Omit when the row has no badges. */
  badges?: React.ReactNode;
  /** Optional trailing action button (the worktree row's open-in-explorer).
   *  Rendered OUTSIDE the inner `<label>` so clicking it does NOT toggle
   *  the checkbox (matches the regression-tested label-vs-button split).
   *  Presence also flips the row's container to `group` so a
   *  `group-hover` style on the action button can fade it in. */
  action?: React.ReactNode;
}

/**
 * One row in the prune list — a checkbox + primary text + status badges,
 * with optional trailing action. Shared between the branch and worktree
 * lists of `<RepoBlock>` so the hover, dim, accessibility-name, and
 * trailing-action contract stays consistent across both surfaces
 * (issue #661 — both rows used to duplicate ~80 lines of identical
 * markup; the duplicated parts were the structural pieces, not the
 * specific badges, so the component takes a `children` + `badges` pair
 * rather than a wider interface of per-context booleans).
 *
 * Accessibility: the checkbox is nested inside a `<label>`, so its
 * accessible name is the concatenation of the `children` text and any
 * badge texts inside the row. The Worktree Manager tests rely on this
 * for `getByRole('checkbox', { name: /feature\/live/i })` — the regex
 * matches the branch name once (it appears verbatim inside the label),
 * not once per badge.
 */
function SelectableRow({
  rowKey,
  title,
  disabled,
  selected,
  onToggle,
  children,
  badges,
  action,
}: SelectableRowProps) {
  return (
    <div
      title={title}
      className={`${action ? 'group ' : ''}flex items-center gap-2 text-xs rounded-md px-1 py-0.5 ${
        disabled
          ? 'opacity-60'
          : 'cursor-pointer hover:bg-bg-overlay/40'
      }`}
    >
      <label
        className={`flex items-center gap-2 flex-1 min-w-0 ${
          disabled ? 'cursor-not-allowed' : 'cursor-pointer'
        }`}
      >
        <input
          type="checkbox"
          checked={selected}
          disabled={disabled}
          onChange={() => onToggle(rowKey)}
          className="accent-accent-cyan disabled:cursor-not-allowed flex-shrink-0"
        />
        <span className="text-text-primary truncate flex-1 min-w-0">{children}</span>
        <span className="flex items-center gap-1 flex-shrink-0">{badges}</span>
      </label>
      {action}
    </div>
  );
}

// ── Repo block (branches / worktrees / remote-tracking) ─────────────────────

interface RepoBlockProps {
  repo: GitRepoPruneInfo;
  selected: Set<string>;
  onToggle: (key: string) => void;
  onPruneRemote: () => void | Promise<void>;
  pruning: boolean;
  selectionDisabled: boolean;
}

/**
 * One mesh-included repo's worth of prune info. Each repo gets its own
 * local-branches + worktrees + remote-tracking section. The path is shown
 * as a one-line header so a mesh with multiple nested repos stays
 * readable.
 */
function RepoBlock({ repo, selected, onToggle, onPruneRemote, pruning, selectionDisabled }: RepoBlockProps) {
  return (
    <div className="space-y-3 rounded-md border border-border-subtle p-3">
      {/* Repo path + open-in-explorer. `min-w-0` lets the path truncate
          under the trailing icon instead of pushing it out of the card. */}
      <div className="flex items-center justify-between gap-2">
        <span
          className="text-2xs font-mono text-text-secondary truncate flex-1 min-w-0"
          title={repo.path}
        >
          {repo.path}
        </span>
        <button
          type="button"
          onClick={() => openInExplorer(repo.path)}
          aria-label={`Open ${repo.path} in file explorer`}
          title="Open in file explorer"
          data-testid={`repo-open-${repo.path}`}
          className="p-1 rounded-md text-text-muted hover:text-accent-cyan hover:bg-bg-card transition-colors flex-shrink-0"
        >
          <FolderOpenIcon className="w-3.5 h-3.5" />
        </button>
      </div>

      {/* Local branches */}
      <div>
        <p className="text-2xs uppercase tracking-wide text-text-muted mb-1">
          Local branches
        </p>
        {repo.local_branches.length === 0 ? (
          <p className="text-xs text-text-muted">None</p>
        ) : (
          <div className="space-y-0.5">
            {repo.local_branches.map((b) => {
              const key = branchKey(repo.path, b.name);
              // Active branches are held by a live agent node — sibling of
              // the worktree `is_active` block. The UI disables the
              // checkbox as the primary defence; the backend
              // `delete_branches` rejects active branches as
              // defence-in-depth.
              //
              // `checked_out_in_worktree` catches the orphan-worktree
              // case `is_active` misses: a branch is HEAD of some
              // working tree on disk (main or linked, live or orphan).
              // Deleting the branch while a worktree holds it would hit
              // libgit2's "current HEAD of a linked repository" error;
              // the safe path is to remove the worktree above, which
              // already cascades to the branch via
              // `remove_one_worktree_and_branch`. Same UI treatment as
              // `is_active` — disabled checkbox, dimmed row, "in
              // worktree" badge pointing at the holding worktree.
              const undeletable = b.is_active || !!b.checked_out_in_worktree;
              const inWorktreeName = b.checked_out_in_worktree
                ? pathDirname(b.checked_out_in_worktree)
                : null;
              return (
                <SelectableRow
                  key={key}
                  rowKey={key}
                  disabled={undeletable || selectionDisabled}
                  selected={selected.has(key)}
                  onToggle={onToggle}
                  title={
                    b.is_active
                      ? 'Active — cannot delete'
                      : b.checked_out_in_worktree
                      ? `Checked out in worktree "${inWorktreeName}" — remove the worktree above to delete this branch`
                      : undefined
                  }
                  badges={
                    <>
                      {b.is_head && (
                        <Badge color="bg-accent-cyan/15 text-accent-cyan" text="HEAD" />
                      )}
                      {b.is_active && (
                        <Badge
                          color="bg-accent-cyan/15 text-accent-cyan"
                          text="active"
                          title="Active — cannot delete"
                        />
                      )}
                      {b.checked_out_in_worktree && (
                        <Badge
                          color="bg-status-warning/15 text-status-warning"
                          text={`in ${inWorktreeName}`}
                          title={`Branch is HEAD of worktree at ${b.checked_out_in_worktree} — delete the worktree to remove this branch`}
                        />
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
                        <span className="text-2xs font-mono text-text-secondary">
                          ↑{b.ahead} ↓{b.behind}
                        </span>
                      )}
                      {b.last_commit_date && (
                        <span className="text-2xs text-text-secondary tabular-nums">
                          {formatDate(b.last_commit_date)}
                        </span>
                      )}
                    </>
                  }
                >
                  {b.name}
                </SelectableRow>
              );
            })}
          </div>
        )}
      </div>

      {/* Worktrees */}
      <div>
        <p className="text-2xs uppercase tracking-wide text-text-muted mb-1">
          Worktrees
        </p>
        {repo.worktrees.length === 0 ? (
          <p className="text-xs text-text-muted">None</p>
        ) : (
          <div className="space-y-0.5">
            {repo.worktrees.map((w) => {
              const key = worktreeKey(w.path);
              const name = pathDirname(w.path);
              // A pool entry is also locked from delete (the worker
              // owns the directory and will refill on next reconcile).
              // The UI disables the checkbox; the backend
              // `delete_worktrees` rejects pool paths as
              // defence-in-depth.
              const undeletable = w.is_active || w.is_pool;
              return (
                <SelectableRow
                  key={key}
                  rowKey={key}
                  disabled={undeletable || selectionDisabled}
                  selected={selected.has(key)}
                  onToggle={onToggle}
                  title={
                    w.is_active
                      ? 'Active — cannot delete'
                      : w.is_pool
                      ? 'Pre-spawn Pool — managed automatically'
                      : w.path
                  }
                  badges={
                    <>
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
                      {w.is_pool && (
                        <Badge
                          color="bg-accent-cyan/15 text-accent-cyan"
                          text="pool"
                          title="Pre-spawn Pool — managed automatically"
                        />
                      )}
                    </>
                  }
                  action={
                    // Per-row open-in-explorer. Renders OUTSIDE the
                    // `<label>` (the component splits the row into a
                    // label-wrapped checkbox + a sibling action so
                    // clicking the icon does NOT toggle the checkbox —
                    // the label-vs-button DOM isolation is
                    // regression-tested).
                    <button
                      type="button"
                      onClick={() => openInExplorer(w.path)}
                      // Full path, not `name` — two worktrees in different
                      // repos with the same directory name would sound
                      // identical to a screen-reader user otherwise.
                      aria-label={`Open ${w.path} in file explorer`}
                      title="Open in file explorer"
                      data-testid={`worktree-open-${key}`}
                      className="p-1 rounded-md text-text-muted opacity-0 group-hover:opacity-100 hover:!opacity-100 hover:text-accent-cyan hover:bg-bg-card transition-opacity flex-shrink-0"
                    >
                      <FolderOpenIcon className="w-3.5 h-3.5" />
                    </button>
                  }
                >
                  {name}
                  {w.branch && (
                    <span className="text-text-secondary"> · {w.branch}</span>
                  )}
                </SelectableRow>
              );
            })}
          </div>
        )}
      </div>

      {/* Remote-tracking branches */}
      {repo.remote_tracking_branches.length > 0 && (
        <div>
          <div className="flex items-center justify-between mb-1">
            <p className="text-2xs uppercase tracking-wide text-text-muted">
              Remote-tracking
            </p>
            <button
              type="button"
              onClick={() => void onPruneRemote()}
              disabled={pruning || selectionDisabled}
              className="text-2xs text-text-muted hover:text-text-primary transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {pruning ? 'Pruning…' : 'Prune'}
            </button>
          </div>
          <div className="space-y-0.5">
            {repo.remote_tracking_branches.map((name) => (
              <div
                key={name}
                className="text-2xs font-mono text-text-secondary px-1 truncate"
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
