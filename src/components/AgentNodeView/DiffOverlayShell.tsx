/**
 * DiffOverlayShell — the shared "what does the overlay look like in the
 * grid" frame: outer wrapper, toolbar (Back to Terminals + breadcrumb +
 * right-aligned mode label), Esc-to-close, and the lens-divergence
 * auto-close. The per-source body (head/base `Diff` view vs PR
 * `PrDiffView`) renders as `children`.
 *
 * Extracted in #421's review: the head/base and PR paths had
 * near-verbatim copies of the toolbar, Esc handler, and auto-close
 * effect. Two-place edits were already drifting (function-name rename),
 * and the load-bearing auto-close predicate had grown a subtle
 * `nodeId !== null` guard for the PR case that the head/base path
 * didn't need. One shell, one place to evolve the toolbar, one
 * place to pin the lens rules.
 *
 * Source-driven variation lives in the props:
 *   - `breadcrumb`: the file/PR/parent label layout, source-specific.
 *   - `modeLabel`: the right-aligned badge ("vs base" / "vs HEAD" /
 *     "PR list" / "PR file"), with its own tooltip.
 *   - `diff`: the `DiffContext` for the lens checks.
 *   - `onClose`: the close action (typically `closeDiff` from the store).
 *   - `children`: the body.
 *
 * Issue #1374 — the shell also owns the Unified/Split view toggle (so the
 * preference is ONE per overlay, not per body) and the per-file quick
 * actions (Stage File / Revert File / Copy Diff) when the body opts in via
 * the `quickActions` prop. The toggle state is bootstrapped from /
 * persisted to localStorage under `buildmesh.diff-view` and threaded down
 * to `<Diff>` through `children` via a render-prop so both source bodies
 * inherit it.
 */

import { useEffect, type ReactNode } from 'react';
import { useUIStore, type DiffContext } from '../../stores/uiStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useDiffViewMode, type DiffViewMode } from '../Diff/Diff';
import { useEscapeKey } from '../../hooks/useEscapeKey';

export interface DiffOverlayShellProps {
  diff: DiffContext;
  breadcrumb: ReactNode;
  /** Right-aligned badge — short text + longer tooltip. */
  modeLabel: { text: string; title: string };
  onClose: () => void;
  /** Issue #1374 — per-file quick actions shown in the toolbar when set
   *  (head/base diffs only; a PR diff has no local file to act on). */
  quickActions?: DiffQuickActions;
  /** The body. Receives the current view mode AND a setter so a child's
   *  per-card toggle stays in sync with the shell's toolbar toggle —
   *  both write to the same localStorage key, but the state must flip in
   *  one place (the shell) so the toolbar button doesn't go out of sync
   *  with the rendered diff. The plain-`ReactNode` form is for bodies
   *  that don't care about the mode (e.g. the PR view); the render-prop
   *  form wires `setMode` through to the body's `<Diff mode={mode}
   *  onModeChange={setMode} />` so card toggles update shell state. */
  children: ReactNode | ((mode: DiffViewMode, setMode: (m: DiffViewMode) => void) => ReactNode);
}

/** The per-file actions #1374 adds to the overlay header. `canStage` /
 *  `canRevert` gate the destructive pair by what the file's diff lens can
 *  actually act on (e.g. a file deleted-in-worktree can be staged but not
 *  re-staged meaningfully — the command handles it; the UI always shows
 *  both for head/base diffs and lets the backend report errors). */
export interface DiffQuickActions {
  onStageFile: () => void;
  onRevertFile: () => void;
  onCopyDiff: () => void;
}

export function DiffOverlayShell({
  diff,
  breadcrumb,
  modeLabel,
  onClose,
  quickActions,
  children,
}: DiffOverlayShellProps) {
  const activeNodeId = useAgentNodeStore((s) => s.activeNodeId);
  const selectedMeshId = useMeshStore((s) => s.selectedMeshId);

  // Issue #1374 — the Unified/Split preference. Single source of truth
  // lives in `useDiffViewMode()` (Diff.tsx) — every caller (this shell,
  // the stacked-review Diff, the inline FileDiffCard toggle) converges
  // on the same module-level store via `useSyncExternalStore`. No more
  // triplicated `useState(loadDiffViewMode)` fighting over localStorage.
  const [viewMode, setViewMode] = useDiffViewMode();
  const toggleViewMode = () => {
    setViewMode(viewMode === 'split' ? 'unified' : 'split');
  };

  // Esc returns to the terminal grid. Bound only while the overlay is
  // mounted, so it never swallows Escape during normal grid use (where
  // agent CLIs read it). The grid is fully covered, so intercepting
  // Escape here is safe. Issue #649 — driven by the shared `useEscapeKey`
  // hook so stacked overlays (Diff above Single-view AgentNodeView)
  // dispatch to the right surface instead of closing both.
  useEscapeKey(() => onClose());

  // Auto-close when the lens diverges from the one the diff was opened
  // under: a different node focused, or a different project selected.
  // Selecting the diff's own mesh (narrowing the sidebar to its project)
  // is not a divergence, hence the `!== null && !== meshId` guard.
  //
  // Node-compare is skipped when `diff.nodeId` is null — that's the
  // shape every PR diff has (its source branch may not even exist
  // locally), and a strict `activeNodeId !== null` comparison would
  // close the overlay on mount the moment any background node is
  // focused (the normal grid state — the regression #421 introduced
  // and then fixed in this same shell).
  useEffect(() => {
    const nodeChanged = diff.nodeId !== null && activeNodeId !== diff.nodeId;
    const meshChanged = selectedMeshId !== null && selectedMeshId !== diff.meshId;
    if (nodeChanged || meshChanged) onClose();
  }, [activeNodeId, selectedMeshId, diff.nodeId, diff.meshId, onClose]);

  return (
    <div
      role="dialog"
      aria-label={modeLabel.title}
      className="absolute inset-0 z-30 flex flex-col bg-bg-base animate-fade-in"
    >
      <div className="flex items-center gap-3 px-3 py-2 border-b border-border-subtle bg-bg-surface shrink-0">
        <button
          type="button"
          onClick={onClose}
          className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-md bg-accent-cyan/10 text-accent-cyan font-medium text-xs hover:bg-accent-cyan/20 transition-colors border border-accent-cyan/20 shrink-0"
        >
          <svg
            width="14"
            height="14"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
            aria-hidden="true"
          >
            <path d="M19 12H5M12 19l-7-7 7-7" />
          </svg>
          Back to Terminals
        </button>
        <div className="flex items-baseline gap-2 min-w-0">{breadcrumb}</div>
        {/* Issue #1374 — Unified/Split toggle, right-aligned with the other
            toolbar controls. `data-testid` pins the persisted-preference
            test; the label mirrors GitHub's own "Split / Unified" naming. */}
        <button
          type="button"
          data-testid="diff-view-toggle"
          onClick={toggleViewMode}
          title={viewMode === 'split' ? 'Switch to unified view' : 'Switch to split view'}
          className="ml-auto inline-flex items-center gap-1 px-2 py-1 rounded-md text-xs text-text-secondary hover:text-accent-cyan hover:bg-bg-card transition-colors border border-border-subtle shrink-0"
        >
          {viewMode === 'split' ? '⬛⬛ Split' : '⬛ Unified'}
        </button>
        {quickActions && (
          <DiffQuickActionsButtons actions={quickActions} />
        )}
        <span
          className={`text-text-muted text-xs shrink-0 ${quickActions ? '' : 'ml-auto'}`}
          title={modeLabel.title}
        >
          {modeLabel.text}
        </span>
      </div>
      <div className="flex-1 min-h-0 overflow-auto">
        {typeof children === 'function' ? children(viewMode, setViewMode) : children}
      </div>
    </div>
  );
}

/** The Stage / Revert / Copy quick-action cluster (issue #1374). Small
 *  icon+text buttons so the toolbar stays scannable; each button's error
 *  handling lives in the action callbacks the head/base body provides. */
function DiffQuickActionsButtons({ actions }: { actions: DiffQuickActions }) {
  return (
    <div className="flex items-center gap-1 shrink-0" data-testid="diff-quick-actions">
      <button
        type="button"
        onClick={actions.onStageFile}
        title="Stage this file"
        className="inline-flex items-center gap-1 px-2 py-1 rounded-md text-xs text-text-secondary hover:text-accent-green hover:bg-bg-card transition-colors border border-border-subtle"
      >
        Stage File
      </button>
      <button
        type="button"
        onClick={actions.onRevertFile}
        title="Revert this file's changes"
        className="inline-flex items-center gap-1 px-2 py-1 rounded-md text-xs text-text-secondary hover:text-accent-red hover:bg-bg-card transition-colors border border-border-subtle"
      >
        Revert File
      </button>
      <button
        type="button"
        onClick={actions.onCopyDiff}
        title="Copy this file's diff to the clipboard"
        className="inline-flex items-center gap-1 px-2 py-1 rounded-md text-xs text-text-secondary hover:text-accent-cyan hover:bg-bg-card transition-colors border border-border-subtle"
      >
        Copy Diff
      </button>
    </div>
  );
}

/** Convenience: produce a close action tied to the `DiffContext` from the
 *  UI store. The shell uses it to wire its own Esc / Back-to-Terminals
 *  button, but call sites that want to also close from a child body
 *  (e.g. a per-file close button) can pull the same action. */
export function useCloseDiff() {
  return useUIStore((s) => s.closeDiff);
}
