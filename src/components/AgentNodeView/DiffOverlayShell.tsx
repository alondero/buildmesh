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
 */

import { useEffect, type ReactNode } from 'react';
import { useUIStore, type DiffContext } from '../../stores/uiStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';

export interface DiffOverlayShellProps {
  diff: DiffContext;
  breadcrumb: ReactNode;
  /** Right-aligned badge — short text + longer tooltip. */
  modeLabel: { text: string; title: string };
  onClose: () => void;
  children: ReactNode;
}

export function DiffOverlayShell({
  diff,
  breadcrumb,
  modeLabel,
  onClose,
  children,
}: DiffOverlayShellProps) {
  const activeNodeId = useAgentNodeStore((s) => s.activeNodeId);
  const selectedMeshId = useMeshStore((s) => s.selectedMeshId);

  // Esc returns to the terminal grid. Bound only while the overlay is
  // mounted, so it never swallows Escape during normal grid use (where
  // agent CLIs read it). The grid is fully covered, so intercepting
  // Escape here is safe.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [onClose]);

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
        <span
          className="ml-auto text-text-muted text-xs shrink-0"
          title={modeLabel.title}
        >
          {modeLabel.text}
        </span>
      </div>
      <div className="flex-1 min-h-0 overflow-auto">{children}</div>
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
