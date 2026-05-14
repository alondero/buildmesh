import { useState, useEffect, useRef, useCallback } from 'react';
import {
  openInEditor,
  type DiffResult,
} from '../../lib/tauri';
import { FileTree } from '../FileTree/FileTree';
import { DiffView } from '../FileTree/DiffView';
import type { FileExplorerContext } from '../../stores/uiStore';

interface FileExplorerPanelProps {
  context: FileExplorerContext;
  width: number;
  onWidthChange: (width: number) => void;
  onClose: () => void;
  nodeName?: string;
  changedCount?: number;
}

export function FileExplorerPanel({
  context,
  width,
  onWidthChange,
  onClose,
  nodeName,
  changedCount,
}: FileExplorerPanelProps) {
  const resizingRef = useRef(false);
  const startXRef = useRef(0);
  const startWidthRef = useRef(0);

  const [selectedFile, setSelectedFile] = useState<string | null>(null);
  const [currentDiff, setCurrentDiff] = useState<DiffResult | null>(null);
  const [isResizing, setIsResizing] = useState(false);

  const handleMouseDown = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      resizingRef.current = true;
      startXRef.current = e.clientX;
      startWidthRef.current = width;
      setIsResizing(true);
    },
    [width]
  );

  useEffect(() => {
    const handleMouseMove = (e: MouseEvent) => {
      if (!resizingRef.current) return;
      const delta = e.clientX - startXRef.current;
      const newWidth = Math.max(280, Math.min(640, startWidthRef.current - delta));
      onWidthChange(newWidth);
    };

    const handleMouseUp = useCallback(() => {
      resizingRef.current = false;
      setIsResizing(false);
    }, []);

    document.addEventListener('mousemove', handleMouseMove);
    document.addEventListener('mouseup', handleMouseUp);
    return () => {
      document.removeEventListener('mousemove', handleMouseMove);
      document.removeEventListener('mouseup', handleMouseUp);
    };
  }, [onWidthChange]);

  const handleBack = useCallback(() => {
    setSelectedFile(null);
    setCurrentDiff(null);
  }, []);

  const handleChangedFileSelect = useCallback(
    async (path: string, diff: DiffResult) => {
      setSelectedFile(path);
      setCurrentDiff(diff);
    },
    []
  );

  const handleUnchangedFileSelect = useCallback(async (path: string) => {
    try {
      await openInEditor(path);
    } catch (e) {
      console.error('Failed to open file in editor:', e);
    }
  }, []);

  // Reset state when context changes
  useEffect(() => {
    setSelectedFile(null);
    setCurrentDiff(null);
  }, [context]);

  const isTreeView = selectedFile === null;
  const headerTitle = (() => {
    switch (context.type) {
      case 'agent':
        return nodeName ? `Agent: ${nodeName} · ${changedCount ?? 0} changed` : 'Agent';
      case 'mesh':
        return 'Mesh';
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
              <span className="text-xs text-text-secondary font-medium truncate">
                {headerTitle}
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
            <FileTree
              rootPath={context.path}
              showGitStatus={context.type === 'agent'}
              onChangedFileSelect={handleChangedFileSelect}
              onUnchangedFileSelect={handleUnchangedFileSelect}
              selectedFile={selectedFile}
              onFileSelect={setSelectedFile}
            />
          </div>
        ) : (
          <div className="flex-1 overflow-hidden">
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