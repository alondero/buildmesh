import { useState, useEffect, useRef } from 'react';
import {
  openInEditor,
  type DiffResult,
} from '../../lib/tauri';
import { FileTree } from '../FileTree/FileTree';
import { DiffView } from '../FileTree/DiffView';
import { ChangedFilesSection } from '../FileTree/ChangedFilesSection';
import { useGitBranchStatus } from '../../hooks/useGitBranchStatus';
import type { FileExplorerContext } from '../../stores/uiStore';

interface FileExplorerPanelProps {
  context: FileExplorerContext;
  width: number;
  onWidthChange: (width: number) => void;
  onClose: () => void;
  nodeName?: string;
  meshName?: string;
  changedCount?: number;
}

export function FileExplorerPanel({
  context,
  width,
  onWidthChange,
  onClose,
  nodeName,
  meshName,
  changedCount,
}: FileExplorerPanelProps) {
  const resizingRef = useRef(false);
  const startXRef = useRef(0);
  const startWidthRef = useRef(0);

  const [selectedFile, setSelectedFile] = useState<string | null>(null);
  const [currentDiff, setCurrentDiff] = useState<DiffResult | null>(null);
  const [isResizing, setIsResizing] = useState(false);
  const [widthRef, setWidthRef] = useState(width);
  const [fileTreeExpanded, setFileTreeExpanded] = useState(true);

  useEffect(() => { setWidthRef(width); }, [width]);

  const handleMouseDown = (e: React.MouseEvent) => {
    e.preventDefault();
    resizingRef.current = true;
    startXRef.current = e.clientX;
    startWidthRef.current = widthRef;
    setIsResizing(true);
  };

  useEffect(() => {
    const handleMouseMove = (e: MouseEvent) => {
      if (!resizingRef.current) return;
      const delta = e.clientX - startXRef.current;
      const newWidth = Math.max(280, Math.min(640, startWidthRef.current - delta));
      onWidthChange(newWidth);
    };

    const handleMouseUp = () => {
      resizingRef.current = false;
      setIsResizing(false);
    };

    document.addEventListener('mousemove', handleMouseMove);
    document.addEventListener('mouseup', handleMouseUp);
    return () => {
      document.removeEventListener('mousemove', handleMouseMove);
      document.removeEventListener('mouseup', handleMouseUp);
    };
  }, [onWidthChange, widthRef]);

  const handleBack = () => {
    setSelectedFile(null);
    setCurrentDiff(null);
  };

  const handleChangedFileSelect = (path: string, diff: DiffResult) => {
    setSelectedFile(path);
    setCurrentDiff(diff);
  };

  const handleUnchangedFileSelect = async (path: string) => {
    try {
      await openInEditor(path);
    } catch (e) {
      console.error('Failed to open file in editor:', e);
    }
  };

  // Reset state when context changes
  useEffect(() => {
    setSelectedFile(null);
    setCurrentDiff(null);
  }, [context]);

  const isTreeView = selectedFile === null;
  const hasGit = context.type === 'agent' || context.type === 'mesh';
  // Subscribe to branch status for git contexts; passes null for userConfig so
  // the hook short-circuits and we render no branch label.
  const { branchStatus } = useGitBranchStatus(hasGit ? context.path : null);
  // Detached HEAD is the default buildmesh agent worktree mode (see
  // buildmesh-worktree-mode memory). `name === "HEAD"` would be uninformative,
  // so we render `detached @ <short-sha>` instead — mirrors `git rev-parse`
  // output. Empty `name` (no commits, yet) hides the label.
  const branchLabel = branchStatus
    ? branchStatus.name === 'HEAD' && branchStatus.short_sha
      ? `detached @ ${branchStatus.short_sha}`
      : branchStatus.name
    : null;
  const headerTitle = (() => {
    switch (context.type) {
      case 'agent':
        return nodeName ? `Agent: ${nodeName} · ${changedCount ?? 0} changed` : 'Agent';
      case 'mesh':
        return meshName ? `Mesh: ${meshName}` : 'Mesh';
      case 'userConfig':
        return 'User Config';
    }
  })();

  return (
    <div className="relative flex h-full" style={{ width }}>
      {/* Resize handle */}
      <div
        onMouseDown={handleMouseDown}
        className={`absolute top-0 left-0 w-1 h-full cursor-col-resize hover:bg-accent-cyan/30 ${
          isResizing ? 'bg-accent-cyan/50' : 'bg-transparent'
        } transition-colors z-10`}
      />

      <div className="w-full bg-bg-surface border-l border-border-subtle flex flex-col h-full overflow-hidden">
        {/* Header */}
        <div
          className="flex items-center justify-between px-3 py-2 border-b border-border-subtle"
          style={{ minHeight: 40 }}
        >
          {isTreeView ? (
            <>
              <span className="text-xs text-text-secondary font-medium truncate flex items-center gap-1.5 min-w-0">
                <span className="truncate">{headerTitle}</span>
                {branchLabel && (
                  <>
                    <span className="text-text-muted flex-shrink-0" aria-hidden="true">·</span>
                    <span
                      className="text-[10px] font-mono text-text-muted whitespace-nowrap flex-shrink-0"
                      title={`Current branch: ${branchLabel}`}
                    >
                      {branchLabel}
                    </span>
                  </>
                )}
              </span>
              <button
                onClick={onClose}
                className="text-text-muted hover:text-text-secondary transition-colors ml-2"
                title="Close"
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
                >
                  <line x1="18" y1="6" x2="6" y2="18" />
                  <line x1="6" y1="6" x2="18" y2="18" />
                </svg>
              </button>
            </>
          ) : (
            <>
              <button
                onClick={handleBack}
                className="text-text-muted hover:text-text-secondary transition-colors mr-2"
                title="Back to tree"
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
                >
                  <line x1="19" y1="12" x2="5" y2="12" />
                  <polyline points="12 19 5 12 12 5" />
                </svg>
              </button>
              <span className="text-xs text-text-secondary font-medium truncate flex-1">
                {selectedFile}
              </span>
              <button
                onClick={onClose}
                className="text-text-muted hover:text-text-secondary transition-colors ml-2"
                title="Close"
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
                >
                  <line x1="18" y1="6" x2="6" y2="18" />
                  <line x1="6" y1="6" x2="18" y2="18" />
                </svg>
              </button>
            </>
          )}
        </div>

        {/* Content */}
        {isTreeView ? (
          <div className="flex-1 overflow-auto">
            {hasGit && (
              <ChangedFilesSection
                rootPath={context.path}
                selectedFile={selectedFile}
                onChangedFileSelect={handleChangedFileSelect}
              />
            )}
            <button
              onClick={() => setFileTreeExpanded(!fileTreeExpanded)}
              className="w-full flex items-center gap-1 px-2 py-1.5 text-[11px] font-medium text-text-secondary hover:bg-bg-card transition-colors border-b border-border-subtle"
            >
              <span className="text-text-muted w-3 text-center text-[10px]">
                {fileTreeExpanded ? '▼' : '▶'}
              </span>
              <span className="flex-1 text-left">File Tree</span>
            </button>
            {fileTreeExpanded && (
              <FileTree
                rootPath={context.path}
                showGitStatus={hasGit}
                onChangedFileSelect={handleChangedFileSelect}
                onUnchangedFileSelect={handleUnchangedFileSelect}
                selectedFile={selectedFile}
                onFileSelect={setSelectedFile}
              />
            )}
          </div>
        ) : (
          // The diff body must scroll when the diff is tall. The container
          // here is a flex child of the panel's flex column above (line 131),
          // and it must itself be a flex column so that DiffView's `flex-1`
          // actually constrains its height — otherwise the diff grows to its
          // content height and the parent clips it without ever scrolling.
          <div className="flex-1 overflow-hidden flex flex-col">
            {currentDiff ? (
              <DiffView diff={currentDiff} />
            ) : (
              <div className="flex items-center justify-center h-full text-text-muted text-xs">
                No changes
              </div>
            )}
          </div>
        )}
      </div>
    </div>
  );
}