import { useState, useCallback } from 'react';
import {
  listDirectory,
  type FileNode,
  diffFileAgainstHead,
  type DiffResult,
} from '../../lib/tauri';
import { useChangedFiles } from '../../hooks/useChangedFiles';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { statusMeta } from '../Diff/Diff';

interface FileTreeProps {
  rootPath: string;
  /** Show M badges on changed files */
  showGitStatus: boolean;
  /** Called when a changed file is selected (for inline diff) */
  onChangedFileSelect?: (path: string, diff: DiffResult) => void;
  /** Called when an unchanged file is selected (to open in editor) */
  onUnchangedFileSelect?: (path: string) => void;
  /** Currently selected file path (to show diff inline) */
  selectedFile: string | null;
  /** Callback to set the selected file */
  onFileSelect: (path: string | null) => void;
}

export function FileTree({
  rootPath,
  showGitStatus,
  onChangedFileSelect,
  onUnchangedFileSelect,
  selectedFile,
  onFileSelect,
}: FileTreeProps) {
  const [treeState, setTreeState] = useState<FileNode | null>(null);
  const [loadingState, setLoadingState] = useState(true);
  const [errorState, setErrorState] = useState<string | null>(null);

  useAsyncEffect((signal) => {
    if (!rootPath) return;
    setLoadingState(true);
    setErrorState(null);
    setTreeState(null);

    listDirectory(rootPath, 4)
      .then((treeData) => {
        if (signal.aborted) return;
        setTreeState(treeData);
        setLoadingState(false);
      })
      .catch((e) => {
        if (signal.aborted) return;
        setErrorState(String(e));
        setLoadingState(false);
      });
  }, [rootPath]);

  // Git status badges come from the shared cache (dedupes with
  // ChangedFilesSection + useMeshGitStatus, and refreshes on GIT_CHANGED — the
  // old combined fetch did neither). Gate the loading view on both so a file's
  // changed/unchanged state is settled before the user can click it.
  const { files: gitFiles, loading: gitLoading } = useChangedFiles(
    showGitStatus ? rootPath || null : null,
  );

  const handleFileClick = useCallback(
    async (path: string, relPath: string, isChanged: boolean) => {
      if (isChanged && onChangedFileSelect) {
        onFileSelect(path);
        try {
          // diff_file_against_head joins session_path + file_path, so the
          // file_path must be relative to the repo root.
          const diff = await diffFileAgainstHead(rootPath, relPath);
          onChangedFileSelect(path, diff);
        } catch (e) {
          console.error('Failed to load diff:', e);
        }
      } else if (onUnchangedFileSelect) {
        onUnchangedFileSelect(path);
      }
    },
    [rootPath, onChangedFileSelect, onUnchangedFileSelect, onFileSelect]
  );

  if (loadingState || gitLoading) {
    return (
      <div className="flex items-center justify-center h-20 text-text-muted text-xs">
        Loading...
      </div>
    );
  }

  if (errorState) {
    return (
      <div className="flex items-center justify-center h-20 text-accent-red text-xs">
        Error: {errorState}
      </div>
    );
  }

  if (!treeState) {
    return (
      <div className="flex items-center justify-center h-20 text-text-muted text-xs">
        No files found
      </div>
    );
  }

  // list_directory returns absolute node paths; get_git_status returns paths
  // relative to the repo root. Key the map by the relative path and reconcile
  // each node by stripping rootPath, so changed files are badged correctly.
  const gitStatusMap = new Map(gitFiles.map((s) => [normalizePath(s.path), s.status]));

  return (
    <div className="text-xs font-mono">
      {treeState.children.map((child) => (
        <TreeNode
          key={child.path}
          node={child}
          rootPath={rootPath}
          gitStatusMap={gitStatusMap}
          depth={0}
          showGitStatus={showGitStatus}
          selectedFile={selectedFile}
          onFileClick={handleFileClick}
        />
      ))}
    </div>
  );
}

/** Normalize separators to `/` and drop any trailing slash. */
function normalizePath(p: string): string {
  return p.replace(/\\/g, '/').replace(/\/+$/, '');
}

/** Path of `abs` relative to `root`, or null if `abs` is not under `root`. */
function relativeToRoot(root: string, abs: string): string | null {
  const nr = normalizePath(root);
  const na = normalizePath(abs);
  if (na === nr) return '';
  const prefix = nr + '/';
  return na.startsWith(prefix) ? na.slice(prefix.length) : null;
}

interface TreeNodeProps {
  node: FileNode;
  rootPath: string;
  gitStatusMap: Map<string, string>;
  depth: number;
  showGitStatus: boolean;
  selectedFile: string | null;
  onFileClick: (path: string, relPath: string, isChanged: boolean) => void;
}

function TreeNode({
  node,
  rootPath,
  gitStatusMap,
  depth,
  showGitStatus,
  selectedFile,
  onFileClick,
}: TreeNodeProps) {
  const [expanded, setExpanded] = useState(false);

  const relPath = relativeToRoot(rootPath, node.path);
  const status = relPath != null ? gitStatusMap.get(relPath) : undefined;
  const isChanged = status !== undefined;
  const isSelected = selectedFile === node.path;

  return (
    <div>
      <div
        className={`
          flex items-center gap-1 px-2 py-0.5 rounded cursor-pointer
          hover:bg-bg-card transition-colors
          ${isSelected ? 'bg-bg-overlay' : ''}
        `}
        style={{ paddingLeft: `${depth * 16 + 8}px` }}
        onClick={() => {
          if (node.is_dir) {
            setExpanded(!expanded);
          } else {
            onFileClick(node.path, relPath ?? node.path, isChanged);
          }
        }}
      >
        {node.is_dir ? (
          <span className="text-text-muted w-3 text-center text-[10px]">
            {expanded ? '▼' : '▶'}
          </span>
        ) : (
          <span className="w-3" />
        )}
        <span className="text-text-muted text-sm">
          {node.is_dir ? '📁' : '📄'}
        </span>
        <span
          className={`flex-1 truncate ${
            node.is_dir ? 'text-text-secondary' : 'text-text-muted'
          }`}
        >
          {node.name}
        </span>
        {showGitStatus && status && (
          <span
            className={`font-bold ${statusMeta(status as Parameters<typeof statusMeta>[0]).color}`}
            title={statusMeta(status as Parameters<typeof statusMeta>[0]).label}
          >
            {statusMeta(status as Parameters<typeof statusMeta>[0]).letter}
          </span>
        )}
      </div>
      {node.is_dir &&
        expanded &&
        node.children.map((child) => (
          <TreeNode
            key={child.path}
            node={child}
            rootPath={rootPath}
            gitStatusMap={gitStatusMap}
            depth={depth + 1}
            showGitStatus={showGitStatus}
            selectedFile={selectedFile}
            onFileClick={onFileClick}
          />
        ))}
    </div>
  );
}