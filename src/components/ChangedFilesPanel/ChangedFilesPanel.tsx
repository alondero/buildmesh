import { useEffect, useState } from 'react';
import { getGitStatus, diffFileAgainstHead, type GitStatus, type DiffResult } from '../../lib/tauri';
import { useGitPathInvalidation } from '../../hooks/useGitPathInvalidation';
import { useResizable } from '../../hooks/useResizable';

interface ChangedFilesPanelProps {
  projectPath: string;
  isOpen: boolean;
  width: number;
  onWidthChange: (width: number) => void;
  onFileSelect: (file: GitStatus, diff: DiffResult) => void;
  onClose: () => void;
}

export function ChangedFilesPanel({ projectPath, isOpen, width, onWidthChange, onFileSelect, onClose }: ChangedFilesPanelProps) {
  const [files, setFiles] = useState<GitStatus[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [diffLoading, setDiffLoading] = useState<string | null>(null);
  // Issue #301: useResizable keeps a `valueRef` updated synchronously per
  // render and snapshots from that ref in mousedown — the previous
  // implementation snapshotted `startWidthRef.current = width` from
  // closed-over state, which gave a stale baseline on the second drag of a
  // fast double-drag and made the handle visibly jump.
  // `side: 'left'` mirrors the original (this panel's resize handle is on
  // its left edge, so dragging right shrinks the panel).
  const { isResizing, handleMouseDown } = useResizable({
    value: width,
    min: 200,
    max: 480,
    side: 'left',
    onChange: onWidthChange,
  });

  const fetchStatus = () => {
    if (!isOpen || !projectPath) return;
    setLoading(true);
    setError(null);
    getGitStatus(projectPath)
      .then(setFiles)
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  };

  useEffect(() => {
    fetchStatus();
  }, [isOpen, projectPath]);

  // Live refresh: re-pull `getGitStatus` whenever the file watcher reports
  // a change in *this* mesh's worktree. Issue #304: the watcher emits the
  // worktree subdir path (or a WSL UNC backslash form) — `pathMatchesGitEvent`
  // inside the primitive normalizes both `path` and `internal_path` and
  // also catches worktree-subdir matches for callers that pass the mesh
  // root. Issue #357: this goes through the cancelled-flag-guarded hook
  // so an event that races a path change (e.g. user switches meshes
  // mid-fetch) is dropped before it can write into the new component's
  // state. Pass `null` while the panel is closed so the hook doesn't
  // install a no-op listener.
  useGitPathInvalidation(
    isOpen && projectPath ? projectPath : null,
    () => fetchStatus(),
  );

  const handleFileClick = async (file: GitStatus) => {
    setDiffLoading(file.path);

    try {
      const diff = await diffFileAgainstHead(projectPath, file.path);
      onFileSelect(file, diff);
    } catch (e) {
      console.error('Failed to load diff:', e);
    } finally {
      setDiffLoading(null);
    }
  };

  const totalFiles = files.length;
  const addedFiles = files.filter(f => f.status === 'added' || f.status === 'untracked').length;
  const modifiedFiles = files.filter(f => f.status === 'modified' || f.status === 'renamed').length;
  const deletedFiles = files.filter(f => f.status === 'deleted').length;

  const statusColors: Record<string, string> = {
    added: 'text-green-400',
    modified: 'text-amber-400',
    deleted: 'text-red-400',
    renamed: 'text-purple-400',
    untracked: 'text-gray-400',
  };

  const statusDots: Record<string, string> = {
    added: 'bg-green-400',
    modified: 'bg-amber-400',
    deleted: 'bg-red-400',
    renamed: 'bg-purple-400',
    untracked: 'bg-gray-400',
  };

  if (!isOpen) return null;

  return (
    <div className="relative flex h-full" style={{ width }}>
      {/* Resize handle */}
      <div
        onMouseDown={handleMouseDown}
        className={`absolute top-0 left-0 w-1 h-full cursor-col-resize hover:bg-accent-cyan/30 ${isResizing ? 'bg-accent-cyan/50' : 'bg-transparent'} transition-colors z-10`}
      />

      {/* Close button */}
      <button
        onClick={onClose}
        className="absolute top-2 right-2 text-text-muted hover:text-text-secondary transition-colors z-10"
        title="Close"
      >
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <line x1="18" y1="6" x2="6" y2="18" />
          <line x1="6" y1="6" x2="18" y2="18" />
        </svg>
      </button>

      <div className="w-full bg-bg-surface border-l border-border-subtle flex flex-col h-full overflow-hidden">
      {/* Header with stats */}
      <div className="px-3 py-2.5 border-b border-border-subtle">
        <div className="flex items-center gap-2 mb-1.5">
          <span className="text-xs text-text-secondary font-medium">
            {totalFiles} file{totalFiles !== 1 ? 's' : ''} changed
          </span>
        </div>
        <div className="flex items-center gap-3 text-[10px]">
          <span className="text-green-400">+{addedFiles}</span>
          <span className="text-amber-400">~{modifiedFiles}</span>
          <span className="text-red-400">-{deletedFiles}</span>
        </div>
      </div>

      {/* File list */}
      <div className="flex-1 overflow-y-auto">
        {loading ? (
          <div className="flex items-center justify-center h-20 text-text-muted text-xs">
            Loading...
          </div>
        ) : error ? (
          <div className="flex items-center justify-center h-20 text-red-400 text-xs">
            {error}
          </div>
        ) : files.length === 0 ? (
          <div className="flex items-center justify-center h-20 text-text-muted text-xs">
            No changes
          </div>
        ) : (
          <div className="py-1">
            {files.map((file) => (
              <button
                key={file.path}
                onClick={() => handleFileClick(file)}
                disabled={diffLoading !== null}
                className={`
                  w-full flex items-center gap-2 px-3 py-1.5 text-left text-xs hover:bg-bg-card transition-colors
                  ${diffLoading === file.path ? 'opacity-50' : ''}
                `}
              >
                <span className={`w-1.5 h-1.5 rounded-full flex-shrink-0 ${statusDots[file.status] || 'bg-gray-400'}`} />
                <span className={`truncate ${statusColors[file.status] || 'text-text-secondary'}`}>
                  {file.path}
                </span>
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
    </div>
  );
}
