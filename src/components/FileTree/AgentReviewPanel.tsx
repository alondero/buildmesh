import { useCallback, useEffect, useRef, useState } from 'react';
import {
  diffNodeAgainstBase,
  openInEditor,
  type DiffResult,
} from '../../lib/tauri';
import { subscribeGitPathInvalidation } from '../../lib/pathInvalidatedCache';
import { Diff, diffTotals } from '../Diff/Diff';
import { FileTree } from './FileTree';

interface AgentReviewPanelProps {
  /** Agent node whose changes-since-branching we review (merge-base, ADR 0005). */
  nodeId: number;
  /** Worktree root, for the browse-the-tree-and-open-in-editor affordance. */
  rootPath: string;
}

/**
 * The cornerstone review surface: every file an agent changed since it
 * branched, stacked in one scroll column with a sticky summary bar and a
 * jump-to-file index. Replaces the old click-one-file-at-a-time flow.
 */
export function AgentReviewPanel({ nodeId, rootPath }: AgentReviewPanelProps) {
  const [diff, setDiff] = useState<DiffResult | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [treeOpen, setTreeOpen] = useState(false);

  // Monotonic token so that, when several fetches overlap (node switch, or a
  // burst of GIT_CHANGED events), only the most recent one is allowed to write
  // state — older in-flight results are dropped.
  const reqId = useRef(0);

  const fetchDiff = useCallback(
    (opts?: { background?: boolean }) => {
      const myId = ++reqId.current;
      // A background refetch keeps the current diff on screen rather than
      // flashing the full-panel loading state.
      if (!opts?.background) setLoading(true);
      setError(null);
      diffNodeAgainstBase(nodeId)
        .then((d) => {
          if (reqId.current !== myId) return;
          setDiff(d);
          setLoading(false);
        })
        .catch((e) => {
          if (reqId.current !== myId) return;
          // Don't blow away a good diff because a background refresh failed;
          // only surface the error when we have nothing else to show.
          if (!opts?.background) setError(String(e));
          setLoading(false);
        });
    },
    [nodeId]
  );

  // Initial load, and a fresh load whenever the node changes.
  useEffect(() => {
    fetchDiff();
  }, [fetchDiff]);

  // Live refresh: the agent keeps editing after the panel mounts, so re-pull
  // the diff whenever the file watcher reports a change in *this* node's
  // worktree. Mirrors how the title-bar git summary chip stays live
  // (useGitSummary) — without this the panel silently goes stale and disagrees
  // with the chip. Issue #304: the watcher emits the worktree subdir path
  // (and a WSL UNC backslash form) — `pathMatchesGitEvent` inside the
  // primitive normalizes both.
  useEffect(() => {
    return subscribeGitPathInvalidation(rootPath, () => {
      fetchDiff({ background: true });
    });
  }, [rootPath, fetchDiff]);

  if (loading) {
    return (
      <div className="flex-1 flex items-center justify-center text-text-muted text-xs">
        Loading changes…
      </div>
    );
  }
  if (error) {
    return (
      <div className="flex-1 flex items-center justify-center text-accent-red text-xs px-3 text-center">
        {error}
      </div>
    );
  }

  const files = diff?.files ?? [];
  const totals = diffTotals(files);

  return (
    <div className="flex-1 min-h-0 overflow-auto">
      {/* Summary bar — stays pinned while the diff scrolls underneath. */}
      <div className="sticky top-0 z-20 flex items-center gap-2 px-3 py-1.5 bg-bg-overlay border-b border-border-subtle text-[11px]">
        <span className="text-text-secondary font-medium">
          {totals.files} {totals.files === 1 ? 'file' : 'files'} changed
        </span>
        {totals.additions > 0 && (
          <span className="text-accent-green font-mono">+{totals.additions}</span>
        )}
        {totals.deletions > 0 && (
          <span className="text-accent-red font-mono">-{totals.deletions}</span>
        )}
        <span
          className="ml-auto text-text-muted"
          title="Changes since this agent branched from its base"
        >
          vs base
        </span>
      </div>

      {files.length === 0 ? (
        <div className="flex items-center justify-center h-40 text-text-muted text-xs">
          No changes vs base branch
        </div>
      ) : (
        // The stacked, highlighted diffs. Each file card has its own sticky
        // header (status letter, path, +/-), so it doubles as the scannable
        // file list — no separate jump index needed in this narrow panel.
        <Diff files={files} />
      )}

      {/* Browse the full tree to open any (even unchanged) file in the editor. */}
      <button
        onClick={() => setTreeOpen(!treeOpen)}
        className="w-full flex items-center gap-1 px-2 py-1.5 text-[11px] font-medium text-text-secondary hover:bg-bg-card transition-colors border-b border-border-subtle"
      >
        <span className="text-text-muted w-3 text-center text-[10px]">
          {treeOpen ? '▼' : '▶'}
        </span>
        <span className="flex-1 text-left">File Tree</span>
      </button>
      {treeOpen && (
        <FileTree
          rootPath={rootPath}
          showGitStatus={false}
          selectedFile={null}
          onFileSelect={() => {}}
          onUnchangedFileSelect={(p) =>
            openInEditor(p).catch((e) =>
              console.error('Failed to open file in editor:', e)
            )
          }
        />
      )}
    </div>
  );
}
