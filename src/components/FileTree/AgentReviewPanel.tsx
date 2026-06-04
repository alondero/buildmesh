import { useEffect, useState } from 'react';
import {
  diffNodeAgainstBase,
  openInEditor,
  type DiffResult,
} from '../../lib/tauri';
import { Diff, diffTotals, diffCardId, statusMeta } from '../Diff/Diff';
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
  const [indexOpen, setIndexOpen] = useState(true);
  const [treeOpen, setTreeOpen] = useState(false);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    setError(null);
    diffNodeAgainstBase(nodeId)
      .then((d) => {
        if (cancelled) return;
        setDiff(d);
        setLoading(false);
      })
      .catch((e) => {
        if (cancelled) return;
        setError(String(e));
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [nodeId]);

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

  const jump = (path: string) => {
    document
      .getElementById(diffCardId(path))
      ?.scrollIntoView({ block: 'start' });
  };

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
        <>
          {/* Jump-to-file index. */}
          <div className="border-b border-border-subtle">
            <button
              onClick={() => setIndexOpen(!indexOpen)}
              className="w-full flex items-center gap-1 px-2 py-1.5 text-[11px] font-medium text-text-secondary hover:bg-bg-card transition-colors"
            >
              <span className="text-text-muted w-3 text-center text-[10px]">
                {indexOpen ? '▼' : '▶'}
              </span>
              <span className="flex-1 text-left">Files</span>
              <span className="text-text-muted text-[10px]">{files.length}</span>
            </button>
            {indexOpen &&
              files.map((file) => {
                const meta = statusMeta(file.status);
                return (
                  <button
                    key={file.path}
                    onClick={() => jump(file.path)}
                    title={file.path}
                    className="w-full flex items-center gap-2 px-2 py-0.5 text-xs font-mono text-left hover:bg-bg-card transition-colors"
                    style={{ paddingLeft: 20 }}
                  >
                    <span
                      className={`font-bold w-3 flex-shrink-0 ${meta.color}`}
                      title={meta.label}
                    >
                      {meta.letter}
                    </span>
                    <span className="flex-1 truncate text-text-muted">
                      {file.path}
                    </span>
                    {file.additions > 0 && (
                      <span className="text-accent-green flex-shrink-0">
                        +{file.additions}
                      </span>
                    )}
                    {file.deletions > 0 && (
                      <span className="text-accent-red flex-shrink-0">
                        -{file.deletions}
                      </span>
                    )}
                  </button>
                );
              })}
          </div>

          {/* The stacked, highlighted diffs. */}
          <Diff files={files} />
        </>
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
