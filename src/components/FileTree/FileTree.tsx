import { useState, useEffect, useCallback, useMemo } from 'react';
import {
  listDirectory,
  type FileNode,
  getGitStatus,
  type GitStatus,
  diffFileAgainstHead,
  type DiffResult,
} from '../../lib/tauri';

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
  const [tree, setTree] = useState<FileNode | null>(null);
  const [gitStatus, setGitStatus] = useState<GitStatus[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!rootPath) return;
    setLoading(true);
    setError(null);

    Promise.all([
      listDirectory(rootPath, 4),
      showGitStatus ? getGitStatus(rootPath) : Promise.resolve(null),
    ])
      .then(([treeData, status]) => {
        setTree(treeData);
        if (status) {
          setGitStatus(status);
        }
        setLoading(false);
      })
      .catch((e) => {
        setError(String(e));
        setLoading(false);
      });
  }, [rootPath, showGitStatus]);

  const handleFileClick = useCallback(
    async (path: string, isChanged: boolean) => {
      if (isChanged && onChangedFileSelect) {
        onFileSelect(path);
        try {
          const diff = await diffFileAgainstHead(rootPath, path);
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

  if (loading) {
    return (
      <div className="flex items-center justify-center h-20 text-text-muted text-xs">
        Loading...
      </div>
    );
  }

  if (error) {
    return (
      <div className="flex items-center justify-center h-20 text-accent-red text-xs">
        {error}
      </div>
    );
  }

  if (!tree) {
    return (
      <div className="flex items-center justify-center h-20 text-text-muted text-xs">
        No files found
      </div>
    );
  }

  const gitStatusMap = useMemo(
    () => new Map(gitStatus.map((s) => [s.path, s.status])),
    [gitStatus]
  );

  return (
    <div className="text-xs font-mono">
      {tree.children.map((child) => (
        <TreeNode
          key={child.path}
          node={child}
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

interface TreeNodeProps {
  node: FileNode;
  gitStatusMap: Map<string, string>;
  depth: number;
  showGitStatus: boolean;
  selectedFile: string | null;
  onFileClick: (path: string, isChanged: boolean) => void;
}

function TreeNode({
  node,
  gitStatusMap,
  depth,
  showGitStatus,
  selectedFile,
  onFileClick,
}: TreeNodeProps) {
  const [expanded, setExpanded] = useState(depth === 0);

  const status = gitStatusMap.get(node.path);
  const isChanged = status !== undefined;
  const isSelected = selectedFile === node.path;

  const statusColors: Record<string, string> = {
    added: 'text-accent-green',
    modified: 'text-accent-amber',
    deleted: 'text-accent-red',
    renamed: 'text-purple-400',
    untracked: 'text-text-muted',
  };

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
            onFileClick(node.path, isChanged);
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
          <span className={`font-bold ${statusColors[status]}`}>M</span>
        )}
      </div>
      {node.is_dir &&
        expanded &&
        node.children.map((child) => (
          <TreeNode
            key={child.path}
            node={child}
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