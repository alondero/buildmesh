/**
 * CenterDiffOverlay — issue #379 (PRD #372).
 *
 * A full-height, full-width diff viewer that temporarily overlays the center
 * terminal grid when the user clicks a changed file in the Probe. Where the
 * Probe's 🔍 review tab stacks *every* changed file into its narrow 360px body,
 * this overlay gives one file the whole center workspace for a spacious read —
 * "playlist-style" review: the Probe stays open and interactive on the right,
 * so clicking another file just swaps the diff in place (the parent
 * `SessionView` re-mounts us with the new `diff` prop).
 *
 * The terminal grid behind the overlay is only *covered*, never torn down — the
 * `TerminalManager` singleton keeps every PTY alive, so "Back to Terminals"
 * returns the user to exactly the grid they left (hard rule: never dispose).
 *
 * Auto-close (acceptance criterion): the diff belongs to a specific lens — the
 * focused node and selected mesh captured in `diff.nodeId` / `diff.meshId` at
 * open time. If the user focuses a *different* background node card, or selects
 * a *different* project in the sidebar, the diff no longer matches what they're
 * looking at, so we close it rather than show a mismatched diff.
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import {
  diffFileAgainstHead,
  diffNodeFileAgainstBase,
  type DiffResult,
  type FileDiff,
} from '../../lib/tauri';
import { useUIStore, type DiffContext } from '../../stores/uiStore';
import { useMeshStore } from '../../stores/meshStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useGitPathInvalidation } from '../../hooks/useGitPathInvalidation';
import { Diff } from '../Diff/Diff';

interface CenterDiffOverlayProps {
  /** The file + lens to render. The parent only mounts us when this is
   *  non-null, so we never have to handle the empty case. */
  diff: DiffContext;
}

export function CenterDiffOverlay({ diff }: CenterDiffOverlayProps) {
  const closeDiff = useUIStore((s) => s.closeDiff);
  const activeNodeId = useAgentNodeStore((s) => s.activeNodeId);
  const selectedMeshId = useMeshStore((s) => s.selectedMeshId);

  // Toolbar label — the file's "parent". Prefer the owning agent node's name;
  // a mesh-scoped diff (Project Files with no node focused) falls back to the
  // mesh name. Looked up live (not stored in the context) so a rename shows
  // through immediately.
  const node = useAgentNodeStore((s) =>
    diff.nodeId === null ? null : s.agentNodes.find((n) => n.id === diff.nodeId) ?? null,
  );
  const meshName = useMeshStore((s) => s.meshesById.get(diff.meshId)?.name ?? null);
  const parentLabel = node?.name ?? meshName ?? 'Workspace';

  const [files, setFiles] = useState<FileDiff[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Monotonic token so an older in-flight fetch (rapid file switching, or a
  // burst of GIT_CHANGED events) can't overwrite the newest result.
  const reqId = useRef(0);

  const fetchDiff = useCallback(
    (opts?: { background?: boolean }) => {
      const myId = ++reqId.current;
      if (!opts?.background) setFiles(null);
      setError(null);
      const promise: Promise<DiffResult> =
        diff.source === 'base' && diff.nodeId !== null
          ? diffNodeFileAgainstBase(diff.nodeId, diff.filePath)
          : diffFileAgainstHead(diff.rootPath, diff.filePath);
      promise
        .then((d) => {
          if (reqId.current !== myId) return;
          setFiles(d.files);
        })
        .catch((e) => {
          if (reqId.current !== myId) return;
          if (!opts?.background) setError(String(e));
        });
    },
    [diff.source, diff.nodeId, diff.filePath, diff.rootPath],
  );

  useEffect(() => {
    fetchDiff();
  }, [fetchDiff]);

  // Live refresh: the agent keeps editing while the overlay is open, so re-pull
  // when the watcher reports a change in this worktree. Mirrors AgentReviewPanel.
  useGitPathInvalidation(diff.rootPath, () => fetchDiff({ background: true }));

  // Esc returns to the terminal grid. Bound only while the overlay is mounted,
  // so it never swallows Escape during normal grid use (where agent CLIs read
  // it). The grid is fully covered, so intercepting Escape here is safe.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') closeDiff();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [closeDiff]);

  // Auto-close when the lens diverges from the one the diff was opened under:
  // a different node focused, or a different project selected. Selecting the
  // diff's own mesh (narrowing the sidebar to its project) is not a divergence,
  // hence the `!== null && !== meshId` guard.
  useEffect(() => {
    const lensChanged =
      activeNodeId !== diff.nodeId ||
      (selectedMeshId !== null && selectedMeshId !== diff.meshId);
    if (lensChanged) closeDiff();
  }, [activeNodeId, selectedMeshId, diff.nodeId, diff.meshId, closeDiff]);

  return (
    <div className="absolute inset-0 z-30 flex flex-col bg-bg-base">
      {/* Toolbar — file metadata + the prominent return button. */}
      <div className="flex items-center gap-3 px-3 py-2 border-b border-border-subtle bg-bg-surface shrink-0">
        <button
          type="button"
          onClick={closeDiff}
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
        <div className="flex items-baseline gap-2 min-w-0">
          <span className="font-mono text-xs text-text-primary truncate" title={diff.filePath}>
            {diff.filePath}
          </span>
          <span className="text-text-muted text-[11px] shrink-0">in</span>
          <span className="text-text-secondary text-xs font-medium truncate" title={parentLabel}>
            {parentLabel}
          </span>
        </div>
        <span
          className="ml-auto text-text-muted text-[11px] shrink-0"
          title={
            diff.source === 'base'
              ? 'Changes since this agent branched from its base'
              : 'Uncommitted changes vs HEAD'
          }
        >
          {diff.source === 'base' ? 'vs base' : 'vs HEAD'}
        </span>
      </div>

      {/* Body — the spacious single-file diff. */}
      <div className="flex-1 min-h-0 overflow-auto">
        {error ? (
          <div className="h-full flex items-center justify-center text-accent-red text-xs px-3 text-center">
            {error}
          </div>
        ) : files === null ? (
          <div className="h-full flex items-center justify-center text-text-muted text-xs">
            Loading diff…
          </div>
        ) : (
          <Diff files={files} />
        )}
      </div>
    </div>
  );
}
