import { useState } from 'react';
import { diffFileAgainstHead, type DiffResult } from '../../lib/tauri';
import { useChangedFiles } from '../../hooks/useChangedFiles';
import { fileDiffStatusMeta } from '../../lib/status';
import { addToast } from '../../stores/toastStore';
import { formatError } from '../../lib/errorUtils';

/** Lucide chevron-right, rotated 90° when the section is expanded.
 *  Matches the FileTree rows' chevron — keeps the probe body's expand
 *  affordance uniform (one SVG glyph, one rotation rule). */
function ChevronRightIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <polyline points="9 18 15 12 9 6" />
    </svg>
  );
}

interface ChangedFilesSectionProps {
  /** Repo root whose uncommitted changes we list. */
  rootPath: string;
  /** Currently open diff path, to highlight the active row. */
  selectedFile: string | null;
  /** Called with the file's relative path and freshly-loaded diff. */
  onChangedFileSelect: (path: string, diff: DiffResult) => void;
}

export function ChangedFilesSection({
  rootPath,
  selectedFile,
  onChangedFileSelect,
}: ChangedFilesSectionProps) {
  // Shared git-status cache: dedupes the fetch with FileTree / useMeshGitStatus
  // and refreshes on GIT_CHANGED (the old hand-rolled fetch did neither — it
  // went stale until remount). A fetch failure surfaces as an empty list, the
  // same as a clean repo, matching the rest of the git-status consumers.
  const { files, loading } = useChangedFiles(rootPath || null);
  const [expanded, setExpanded] = useState(true);

  // get_git_status returns paths relative to the repo root, which is exactly
  // what diff_file_against_head expects — no conversion needed.
  const handleClick = async (path: string) => {
    try {
      const diff = await diffFileAgainstHead(rootPath, path);
      onChangedFileSelect(path, diff);
    } catch (e) {
      // Issue #1245 — surface the failure instead of swallowing it. A
      // worktree lock, oversized binary, or transient git error used to
      // leave the row highlight looking like an open diff with nothing
      // actually loaded. The toast matches the `addToast(provider, ...)`
      // convention used by NodeItem.tsx (e.g. `'Regenerate failed'`,
      // `'Restart failed'`) so a user sees the same toast shape across
      // every diff-load / spawn / restart failure in the app.
      addToast('Review', `Failed to load diff for ${path}: ${formatError(e)}`, 'error');
    }
  };

  return (
    <div className="border-b border-border-subtle">
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full flex items-center gap-1 px-2 py-1.5 text-xs font-medium text-text-secondary hover:bg-bg-card transition-colors"
      >
        <span
          className={`w-3 h-3 flex items-center justify-center text-text-muted transition-transform ${
            expanded ? 'rotate-90' : ''
          }`}
        >
          <ChevronRightIcon className="w-3 h-3" />
        </span>
        <span className="flex-1 text-left">Changed Files</span>
        <span className="text-text-muted text-2xs">{files.length}</span>
      </button>
      {expanded && (
        <div className="pb-1">
          {loading ? (
            <div className="px-3 py-1.5 text-text-muted text-xs">Loading…</div>
          ) : files.length === 0 ? (
            <div className="px-3 py-1.5 text-text-muted text-xs">No changes</div>
          ) : (
            files.map((file) => {
              const meta = fileDiffStatusMeta(file.status);
              return (
              <button
                key={file.path}
                onClick={() => handleClick(file.path)}
                title={file.path}
                className={`
                  w-full flex items-center gap-2 px-2 py-0.5 text-xs font-mono text-left
                  hover:bg-bg-card transition-colors
                  ${selectedFile === file.path ? 'bg-bg-overlay' : ''}
                `}
                style={{ paddingLeft: 20 }}
              >
                <span className={`font-bold w-3 flex-shrink-0 ${meta.color}`} title={meta.label}>
                  {meta.letter}
                </span>
                <span className="flex-1 truncate text-text-secondary">{file.path}</span>
                <span className="text-accent-green flex-shrink-0">+{file.additions}</span>
                <span className="text-accent-red flex-shrink-0">-{file.deletions}</span>
              </button>
              );
            })
          )}
        </div>
      )}
    </div>
  );
}
