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

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useMeshHealth } from '../../hooks/useMeshHealth';
import { useMeshRecovery } from '../../hooks/useMeshRecovery';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { ConfirmDialog } from '../ConfirmDialog/ConfirmDialog';
import {
  deleteBranches,
  deleteWorktrees,
  getGitPruneInfo,
  getMeshProperties,
  openInFileManager,
  pruneRemoteTracking,
  updateMeshColumn,
  updateMeshPoolSize,
  updateMeshUseWorktree,
  updateWorktreeBaseRef,
  type BranchInfo,
  type GitRepoPruneInfo,
  type HoldingWorktree,
  type MeshHealth,
  type WorktreeInfo,
} from '../../lib/tauri';

/** Lucide folder-open. */
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
// current HEAD, has nothing uncommitted to lose, isn't held by an active
// agent node, and is either fully merged into main or orphaned (its upstream
// remote branch is gone). The `!is_active` clause mirrors the worktree rule
// — a live agent on a branch must close the node first before the branch
// becomes prunable. A worktree is recommended when its branch no longer
// exists (stale) and no agent is using it. Mirrors the legacy section's logic.
const isRecommendedBranch = (b: BranchInfo) =>
  !b.is_head &&
  !b.has_uncommitted &&
  !b.is_active &&
  (b.is_merged_into_main === true || b.is_orphan);

const isRecommendedWorktree = (w: WorktreeInfo) => !w.is_active && w.is_stale;

// ── Configuration card (issue #451) ─────────────────────────────────────────

/**
 * The default worktree mode for a fresh agent node. Local pin that must
 * agree with `DEFAULT_WORKTREE_MODE` in `src-tauri/src/agent/spawn.rs`
 * (per the cross-language default coupling pattern — see ADR
 * follow-up). When the `meshes.worktree_mode` column is missing or
 * `null` on the wire, this is the value the form falls back to.
 */
const DEFAULT_WORKTREE_MODE = 'branched';

/**
 * Wire-shape form values for the Starting point radio. The on-the-wire
 * value (passed to `update_worktree_base_ref`) is the `wire` field —
 * `'origin/main'` for fresh sessions, `'HEAD'` for resuming. The legacy
 * `MeshPropertiesPanel` (deleted in #380) used the same two options.
 */
type BaseRefForm = 'fresh' | 'head';

const BASEREF_OPTIONS: { value: BaseRefForm; label: string; wire: 'origin/main' | 'HEAD' }[] = [
  { value: 'fresh', label: 'Fresh — start new session (origin/<default>)', wire: 'origin/main' },
  { value: 'head', label: 'Head — resume last session (HEAD)', wire: 'HEAD' },
];

/**
 * Wire-shape form values for the Worktree mode radio. Mirrors the
 * legacy `MeshPropertiesPanel` options verbatim. `branched` is the
 * default per `DEFAULT_WORKTREE_MODE` above.
 */
type WorktreeModeForm = 'branched' | 'detached';

const WORKTREE_MODE_OPTIONS: { value: WorktreeModeForm; label: string }[] = [
  { value: 'branched', label: 'Branched — actual git branch per worktree (default)' },
  { value: 'detached', label: 'Detached — detached HEAD worktree' },
];

// Mappers between the form's two-option enum and the open-ended
// string values stored in `meshes.base_ref` / `meshes.worktree_mode`.
// We intentionally collapse anything other than the canonical values
// to the form's default — matches the legacy panel's
// `config.base_ref === 'HEAD' ? 'head' : 'fresh'` rule and the
// `config.worktree_mode ?? DEFAULT_WORKTREE_MODE` fallback.
const wireToFormBaseRef = (wire: string | null): BaseRefForm =>
  wire === 'HEAD' ? 'head' : 'fresh';
const wireToFormMode = (wire: string | null): WorktreeModeForm =>
  wire === 'detached' ? 'detached' : DEFAULT_WORKTREE_MODE;
const formToWireBaseRef = (form: BaseRefForm): 'origin/main' | 'HEAD' =>
  form === 'head' ? 'HEAD' : 'origin/main';

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

  // Configuration card state (issue #451). The three fields are
  // initialised to the legacy `MeshPropertiesPanel` defaults (use a
  // worktree, start a fresh session, branched mode). The load effect
  // below replaces them with the persisted values from the wire.
  //
  // `saveError` is scoped to the Configuration card only — the
  // existing `error` channel is reserved for prune / recovery /
  // remote-tracking failures and renders below the toolbar. Keeping
  // the two channels separate prevents a stale prune error from
  // getting clobbered by a config save (or vice versa).
  const [useWorktree, setUseWorktree] = useState(true);
  const [baseRef, setBaseRef] = useState<BaseRefForm>('fresh');
  const [worktreeMode, setWorktreeMode] = useState<WorktreeModeForm>(DEFAULT_WORKTREE_MODE);
  // Pre-spawn Worktree Pool size (issue #611). `0` = pool off,
  // `1..=5` = target. The toggle's `checked` is derived from
  // `preSpawnPoolSize > 0`; the size input clamps to 1..5 and is
  // disabled when the toggle is off. The pool worker
  // (`services::warm_pool`) reads this column on startup + after each
  // claim, draining excess and filling up to this target.
  const [preSpawnPoolSize, setPreSpawnPoolSize] = useState(0);
  const [saveError, setSaveError] = useState<string | null>(null);

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

  // Configuration card load (issue #451). Mirrors the dep discipline
  // from `MeshPropertiesTab.tsx:143-167`: the dep set is intentionally
  // `[activeMeshId, activePath]` only — adding the form state (e.g.
  // `useWorktree`) would re-fire on every save and clobber the user's
  // in-flight edits to `baseRef` / `worktreeMode` with the just-saved
  // values. On a load failure we keep the stale form state rather
  // than showing an error — the user can still toggle the controls,
  // and a subsequent save will retry the load path on the next
  // mesh switch.
  useAsyncEffect(
    (signal) => {
      if (activeMeshId === null) return;
      getMeshProperties(activeMeshId)
        .then((config) => {
          if (signal.aborted) return;
          setUseWorktree(config.use_worktree);
          setBaseRef(wireToFormBaseRef(config.base_ref));
          setWorktreeMode(wireToFormMode(config.worktree_mode));
          setPreSpawnPoolSize(config.pre_spawn_pool_size);
        })
        .catch(() => {
          // Swallow — keep the existing form state. Mirrors the legacy
          // "form mirrors user intent" rule and matches the
          // MeshPropertiesTab's load-failure behaviour.
        });
    },
    [activeMeshId, activePath],
  );

  // Configuration card save handlers. Each: (1) update form state
  // optimistically, (2) clear any prior save error, (3) call the
  // typed wrapper, (4) on reject, surface the error inline WITHOUT
  // reverting the form. The "do not revert" rule matches the legacy
  // panel — for a binary control like a checkbox, reverting would
  // silently undo the user's click and they would have no idea why.
  const handleToggleUseWorktree = useCallback(
    async (next: boolean) => {
      if (activeMeshId === null) return;
      setUseWorktree(next);
      setSaveError(null);
      try {
        await updateMeshUseWorktree(activeMeshId, next);
      } catch (e) {
        setSaveError(`Failed to update use_worktree: ${String(e)}`);
      }
    },
    [activeMeshId],
  );

  const handleChangeBaseRef = useCallback(
    async (next: BaseRefForm) => {
      if (activeMeshId === null) return;
      setBaseRef(next);
      setSaveError(null);
      try {
        await updateWorktreeBaseRef(activeMeshId, formToWireBaseRef(next));
      } catch (e) {
        setSaveError(`Failed to update base_ref: ${String(e)}`);
      }
    },
    [activeMeshId],
  );

  const handleChangeWorktreeMode = useCallback(
    async (next: WorktreeModeForm) => {
      if (activeMeshId === null) return;
      setWorktreeMode(next);
      setSaveError(null);
      try {
        await updateMeshColumn(activeMeshId, 'worktree_mode', next);
      } catch (e) {
        setSaveError(`Failed to update worktree_mode: ${String(e)}`);
      }
    },
    [activeMeshId],
  );

  // Pre-spawn pool size save (issue #611). Single handler for both the
  // toggle (which sends 0 or 1) and the size input (which sends 1..5);
  // the backend enforces the `0..=5` invariant and rejects otherwise.
  // Mirrors the other save handlers' "do not revert on save failure"
  // rule — if the user shrinks the pool to 5 and the backend rejects,
  // the form keeps their typed value so the failure message is
  // actionable.
  const handleChangePoolSize = useCallback(
    async (next: number) => {
      if (activeMeshId === null) return;
      const clamped = Math.max(0, Math.min(5, Math.trunc(next) || 0));
      setPreSpawnPoolSize(clamped);
      setSaveError(null);
      try {
        await updateMeshPoolSize(activeMeshId, clamped);
      } catch (e) {
        setSaveError(`Failed to update pool size: ${String(e)}`);
      }
    },
    [activeMeshId],
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
    // `activeMeshId` is the mesh-scoped key `deleteBranches` needs to
    // scope the active-branch guard. The tab early-returns at the
    // bottom when no mesh is active, but TypeScript can't narrow that
    // across the closure boundary here, so guard locally. We close the
    // dialog + surface an error rather than silently freezing the modal
    // — a stale closure (mesh switched after the dialog opened) would
    // otherwise leave the user staring at a non-responsive confirmation.
    if (activeMeshId === null) {
      setConfirming(false);
      setError('No active mesh — cannot delete.');
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

      {/* Configuration card (issue #451) — sits above the prune
          toolbar so the user sees the "what kind of worktree does
          this mesh use" controls before they decide what to prune.
          The saveError slot below the card mirrors the prune-error
          slot at the bottom of the toolbar; the two channels are
          kept separate so a config save failure doesn't get
          clobbered by a later prune error (or vice versa). */}
      <ConfigurationCard
        useWorktree={useWorktree}
        baseRef={baseRef}
        worktreeMode={worktreeMode}
        preSpawnPoolSize={preSpawnPoolSize}
        onToggleUseWorktree={handleToggleUseWorktree}
        onChangeBaseRef={handleChangeBaseRef}
        onChangeWorktreeMode={handleChangeWorktreeMode}
        onChangePoolSize={handleChangePoolSize}
      />
      {saveError && (
        <p className="text-xs text-status-error break-words">{saveError}</p>
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
      {/* Issue #657 — success channel for prune_remote_tracking. Same
          inline style + 4s auto-clear as the `gitSync` pattern in
          `MeshItem.tsx` (cleaner than wiring the global toast system
          for one button). Styled `text-status-success` to mirror the
          red error above. */}
      {successMessage && (
        <p className="text-xs text-status-success whitespace-pre-wrap break-words">{successMessage}</p>
      )}

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
                // Mirrors the legacy `handleDelete` pattern at
                // `WorktreeManagerTab.tsx:478-486` — accumulate partial-
                // failure errors, refresh in `finally`, then restore the
                // error AFTER `load()`'s own `setError(null)` runs. Keeps
                // a post-prune refresh failure from being mislabelled
                // "Prune failed:" (review finding A1/C1).
                let pruneError: string | null = null;
                try {
                  const message = await pruneRemoteTracking(repo.path);
                  // git returns its (possibly empty) report on stderr.
                  // The Rust side already trims; empty stderr means
                  // "nothing was pruned" — surface a fallback so the
                  // user always sees something.
                  const display = message || 'Remote-tracking refs pruned.';
                  setSuccessMessage(display);
                  if (successTimerRef.current) clearTimeout(successTimerRef.current);
                  successTimerRef.current = setTimeout(() => setSuccessMessage(null), 4000);
                } catch (e) {
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
                  pruneError = `Prune failed: ${String(e)}`;
                } finally {
                  setPruningPaths((prev) => {
                    const next = new Set(prev);
                    next.delete(repo.path);
                    return next;
                  });
                  // Refresh the prune-info list after the prune settles.
                  // `load()`'s own catch writes refresh errors to the
                  // shared `error` channel WITHOUT the "Prune failed:"
                  // prefix — refresh failures shouldn't be mislabelled
                  // as prune failures.
                  await load();
                  // Restore the prune error AFTER `load()`'s own
                  // `setError(null)` has run (so the prune message
                  // doesn't get clobbered by a successful refresh).
                  if (pruneError) setError(pruneError);
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
      return 'already on the Base Ref';
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
          title={restoreBlockedBy ?? 'Restore the mesh root to the Base Ref'}
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
  pruning: boolean;
}

/**
 * One mesh-included repo's worth of prune info. Each repo gets its own
 * local-branches + worktrees + remote-tracking section. The path is shown
 * as a one-line header so a mesh with multiple nested repos stays
 * readable.
 */
function RepoBlock({ repo, selected, onToggle, onPruneRemote, pruning }: RepoBlockProps) {
  return (
    <div className="space-y-3 rounded border border-border-subtle p-3">
      {/* Repo path + open-in-explorer. `min-w-0` lets the path truncate
          under the trailing icon instead of pushing it out of the card. */}
      <div className="flex items-center justify-between gap-2">
        <span
          className="text-[10px] font-mono text-text-muted truncate flex-1 min-w-0"
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
          className="p-1 rounded text-text-muted hover:text-accent-cyan hover:bg-bg-card transition-colors flex-shrink-0"
        >
          <FolderOpenIcon className="w-3.5 h-3.5" />
        </button>
      </div>

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
              // Active branches are held by a live agent node — sibling of
              // the worktree `is_active` block. The UI disables the
              // checkbox as the primary defence; the backend
              // `delete_branches` rejects active branches as
              // defence-in-depth.
              const undeletable = b.is_active;
              return (
                <div
                  key={key}
                  title={b.is_active ? 'Active — cannot delete' : undefined}
                  className={`flex items-center gap-2 text-xs rounded px-1 py-0.5 ${
                    undeletable
                      ? 'opacity-60'
                      : 'cursor-pointer hover:bg-bg-overlay/40'
                  }`}
                >
                  <label
                    className={`flex items-center gap-2 flex-1 min-w-0 ${
                      undeletable ? 'cursor-not-allowed' : 'cursor-pointer'
                    }`}
                  >
                    <input
                      type="checkbox"
                      checked={selected.has(key)}
                      disabled={undeletable}
                      onChange={() => onToggle(key)}
                      className="accent-accent-cyan disabled:cursor-not-allowed flex-shrink-0"
                    />
                    <span className="text-text-primary truncate flex-1">{b.name}</span>
                    <span className="flex items-center gap-1 flex-shrink-0">
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
                </div>
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
              // A pool entry is also locked from delete (the worker
              // owns the directory and will refill on next reconcile).
              // The UI disables the checkbox; the backend
              // `delete_worktrees` rejects pool paths as
              // defence-in-depth.
              const undeletable = w.is_active || w.is_pool;
              return (
                <div
                  key={key}
                  title={
                    w.is_active
                      ? 'Active — cannot delete'
                      : w.is_pool
                      ? 'Pre-spawn Pool — managed automatically'
                      : w.path
                  }
                  className={`group flex items-center gap-2 text-xs rounded px-1 py-0.5 ${
                    undeletable
                      ? 'opacity-60'
                      : 'cursor-pointer hover:bg-bg-overlay/40'
                  }`}
                >
                  <label
                    className={`flex items-center gap-2 flex-1 min-w-0 ${
                      undeletable ? 'cursor-not-allowed' : 'cursor-pointer'
                    }`}
                  >
                    <input
                      type="checkbox"
                      checked={selected.has(key)}
                      disabled={undeletable}
                      onChange={() => onToggle(key)}
                      className="accent-accent-cyan disabled:cursor-not-allowed flex-shrink-0"
                    />
                    <span className="text-text-primary truncate flex-1 min-w-0">
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
                      {w.is_pool && (
                        <Badge
                          color="bg-accent-cyan/15 text-accent-cyan"
                          text="pool"
                          title="Pre-spawn Pool — managed automatically"
                        />
                      )}
                    </span>
                  </label>
                  {/* Per-row open-in-explorer. Outside the inner `<label>`
                      so clicking it doesn't toggle the row's checkbox —
                      the label-vs-button DOM split isolates the click
                      target (regression-tested). */}
                  <button
                    type="button"
                    onClick={() => openInExplorer(w.path)}
                    // Full path, not `name` — two worktrees in different
                    // repos with the same directory name would sound
                    // identical to a screen-reader user otherwise.
                    aria-label={`Open ${w.path} in file explorer`}
                    title="Open in file explorer"
                    data-testid={`worktree-open-${key}`}
                    className="p-1 rounded text-text-muted opacity-0 group-hover:opacity-100 hover:!opacity-100 hover:text-accent-cyan hover:bg-bg-card transition-opacity flex-shrink-0"
                  >
                    <FolderOpenIcon className="w-3.5 h-3.5" />
                  </button>
                </div>
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
              disabled={pruning}
              className="text-[10px] text-text-muted hover:text-text-primary transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
            >
              {pruning ? 'Pruning…' : 'Prune'}
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

// ── Configuration card (issue #451) ─────────────────────────────────────────

interface ConfigurationCardProps {
  useWorktree: boolean;
  baseRef: BaseRefForm;
  worktreeMode: WorktreeModeForm;
  /** Per-mesh pre-spawn pool target (`0` = off, `1..=5`). The toggle's
   *  `checked` is derived from `preSpawnPoolSize > 0`. */
  preSpawnPoolSize: number;
  onToggleUseWorktree: (next: boolean) => void;
  onChangeBaseRef: (next: BaseRefForm) => void;
  onChangeWorktreeMode: (next: WorktreeModeForm) => void;
  onChangePoolSize: (next: number) => void;
}

/**
 * Ports the worktree-config sub-section that used to live at the top
 * of the legacy `MeshPropertiesPanel` (deleted in #380). The card
 * surfaces four controls:
 *
 *   1. **Use worktree** — checkbox. When unchecked, the radio groups
 *      + the pre-spawn pool block collapse (matches the legacy
 *      panel's `{form.useWorktree && <div className="pl-4 border-l …">…}`
 *      block).
 *   2. **Pre-spawn warm worktrees** — checkbox + size input (issue
 *      #611). Toggle is derived from `preSpawnPoolSize > 0`; the size
 *      input (1..5) is disabled when the toggle is off. Toggling on
 *      defaults the size to 1; toggling off sets it to 0.
 *   3. **Starting point** — `<fieldset>` with two radios that map
 *      Fresh ↔ `origin/main` and Head ↔ `HEAD` on the wire. The
 *      fieldset/legend pairing gives the radios a real `radiogroup`
 *      ARIA role (the test harness resolves individual radios by
 *      their label via `getByRole('radio', { name: … })`).
 *   4. **Worktree mode** — same pattern, branched vs detached.
 *
 * All saves fire on change through the parent's typed wrappers — no
 * raw `invoke` (preserves the `tauri-ipc-seam` ratchet at
 * `tests/unit/tauri-ipc-seam.test.ts`). The card is intentionally
 * "dumb" — it owns no state, all the form values and the
 * save-handler callbacks come from the parent. That keeps the
 * "form mirrors user intent" + "do not revert on save failure"
 * guarantees in one place.
 *
 * The wrapping `<label>` pattern (option label + nested input)
 * is intentional: it makes `getByLabelText('Use worktree')` resolve
 * the checkbox in the test harness without needing a separate
 * `id`/`htmlFor` wire. Radio options use the same pattern so
 * `getByRole('radio', { name: /Fresh/ })` finds them. The pool
 * toggle follows the same `<label>` pattern so
 * `getByLabelText('Pre-spawn warm worktrees')` resolves it.
 */
function ConfigurationCard({
  useWorktree,
  baseRef,
  worktreeMode,
  preSpawnPoolSize,
  onToggleUseWorktree,
  onChangeBaseRef,
  onChangeWorktreeMode,
  onChangePoolSize,
}: ConfigurationCardProps) {
  const poolEnabled = preSpawnPoolSize > 0;
  // Display value for the size number input. When the toggle is off,
  // show 1 (the spec's default — the user hasn't picked a size yet, but
  // we need a placeholder that the disabled input can render). When the
  // toggle is on, `preSpawnPoolSize` is `>= 1` by the poolEnabled guard,
  // so the value is already a valid 1..5. The `|| 1` short-circuits the
  // edge case where the IPC clamp rejects a write and the form keeps
  // the stale 0 (defensive; `poolEnabled` would already be false).
  const poolInputDisplay = preSpawnPoolSize || 1;
  return (
    <div className="rounded border border-border-subtle p-3 space-y-3">
      <p className="text-[10px] uppercase tracking-wide text-text-muted">
        Worktree configuration
      </p>

      {/* Use worktree checkbox — the gate. When unchecked, the radio
          groups + the pre-spawn pool block below collapse. */}
      <label className="flex items-center gap-2 text-xs text-text-primary cursor-pointer">
        <input
          type="checkbox"
          checked={useWorktree}
          onChange={(e) => onToggleUseWorktree(e.target.checked)}
          className="accent-accent-cyan"
        />
        <span>Use worktree</span>
      </label>

      {useWorktree && (
        <div className="pl-4 border-l border-border-subtle space-y-3">
          {/* Pre-spawn pool (issue #611). The toggle is derived from
              `preSpawnPoolSize > 0`; the size input is disabled when
              the toggle is off. Sits ABOVE Starting point / Worktree
              mode because it's a worktree-strategy decision — the user
              should pick the warm-pool size before deciding where the
              base ref sits. */}
          <div className="space-y-2">
            <label className="flex items-center gap-2 text-xs text-text-primary cursor-pointer">
              <input
                type="checkbox"
                checked={poolEnabled}
                onChange={(e) =>
                  // Toggle off → 0 (disables the pool). Toggle on → 1
                  // (the spec's default; the size input then lets the
                  // user pick 2..5). The save fires immediately so the
                  // pool worker's next reconcile sees the new target.
                  onChangePoolSize(e.target.checked ? 1 : 0)
                }
                className="accent-accent-cyan"
              />
              <span>Pre-spawn warm worktrees</span>
            </label>
            <label
              className={`flex items-center gap-2 text-xs pl-4 ${
                poolEnabled
                  ? 'text-text-primary cursor-pointer'
                  : 'text-text-muted cursor-not-allowed'
              }`}
            >
              <span>Pool size</span>
              <input
                type="number"
                min={1}
                max={5}
                step={1}
                value={poolInputDisplay}
                disabled={!poolEnabled}
                onChange={(e) => {
                  // Clamp at the IPC boundary AND the input handler so
                  // an out-of-range typed value doesn't bounce through
                  // a save round-trip just to get rejected.
                  const n = Number(e.target.value);
                  if (Number.isFinite(n) && n >= 1 && n <= 5) {
                    onChangePoolSize(Math.trunc(n));
                  }
                }}
                className="w-12 px-1 py-0.5 rounded border border-border-subtle bg-bg-overlay text-text-primary disabled:opacity-50 disabled:cursor-not-allowed"
                title={
                  poolEnabled
                    ? '1 = one warm worktree pre-cut; 5 = five'
                    : 'Toggle "Pre-spawn warm worktrees" to set a size'
                }
              />
            </label>
            <p className="text-[10px] text-text-muted pl-4">
              Pre-warm worktree directories at startup. Manual spawns
              land on a pre-cut directory in &lt;500ms instead of ~11s.
            </p>
          </div>

          {/* Starting point — Fresh / Head */}
          <fieldset className="border-0 p-0 m-0 space-y-2">
            <legend className="block text-xs text-text-muted mb-1">
              Starting point
            </legend>
            {BASEREF_OPTIONS.map((o) => (
              <label
                key={o.value}
                className="flex items-start gap-2 text-xs text-text-primary cursor-pointer"
              >
                <input
                  type="radio"
                  name="wt-cfg-baseref"
                  value={o.value}
                  checked={baseRef === o.value}
                  onChange={() => onChangeBaseRef(o.value)}
                  className="mt-0.5 accent-accent-cyan"
                />
                <span>{o.label}</span>
              </label>
            ))}
          </fieldset>

          {/* Worktree mode — Branched / Detached */}
          <fieldset className="border-0 p-0 m-0 space-y-2">
            <legend className="block text-xs text-text-muted mb-1">
              Worktree mode
            </legend>
            {WORKTREE_MODE_OPTIONS.map((o) => (
              <label
                key={o.value}
                className="flex items-start gap-2 text-xs text-text-primary cursor-pointer"
              >
                <input
                  type="radio"
                  name="wt-cfg-mode"
                  value={o.value}
                  checked={worktreeMode === o.value}
                  onChange={() => onChangeWorktreeMode(o.value)}
                  className="mt-0.5 accent-accent-cyan"
                />
                <span>{o.label}</span>
              </label>
            ))}
          </fieldset>
        </div>
      )}
    </div>
  );
}
