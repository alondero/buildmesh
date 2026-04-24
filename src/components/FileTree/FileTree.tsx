import { useState, useEffect } from 'react';
import { listDirectory, FileNode, getGitStatus, GitStatus } from '../../lib/tauri';

interface FileTreeProps {
  sessionPath: string;
  onFileClick?: (path: string) => void;
}

export function FileTree({ sessionPath, onFileClick }: FileTreeProps) {
  const [tree, setTree] = useState<FileNode | null>(null);
  const [gitStatus, setGitStatus] = useState<GitStatus[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    if (!sessionPath) return;
    setLoading(true);
    setError(null);

    Promise.all([
      listDirectory(sessionPath, 4).catch((e) => {
        setError(String(e));
        return null;
      }),
      getGitStatus(sessionPath).catch(() => [] as GitStatus[]),
    ]).then(([treeData, status]) => {
      setTree(treeData);
      setGitStatus(status || []);
      setLoading(false);
    });
  }, [sessionPath]);

  if (loading) {
    return <div className="text-xs text-[#666] animate-pulse">Loading files...</div>;
  }

  if (error) {
    return <div className="text-xs text-[#ef4444]">Error: {error}</div>;
  }

  if (!tree) {
    return <div className="text-xs text-[#666]">No files found</div>;
  }

  const gitStatusMap = new Map(gitStatus.map(s => [s.path, s.status]));

  return (
    <div className="text-xs font-mono">
      {tree.children.map(child => (
        <TreeNode
          key={child.path}
          node={child}
          gitStatusMap={gitStatusMap}
          depth={0}
          onFileClick={onFileClick}
        />
      ))}
    </div>
  );
}

function TreeNode({
  node,
  gitStatusMap,
  depth,
  onFileClick,
}: {
  node: FileNode;
  gitStatusMap: Map<string, string>;
  depth: number;
  onFileClick?: (path: string) => void;
}) {
  const [expanded, setExpanded] = useState(depth < 2);

  const status = gitStatusMap.get(node.path);
  const statusColors: Record<string, string> = {
    added: 'text-[#22c55e]',
    modified: 'text-[#f59e0b]',
    deleted: 'text-[#ef4444]',
    renamed: 'text-[#8b5cf6]',
    untracked: 'text-[#6b7280]',
  };

  const statusIcons: Record<string, string> = {
    added: '+',
    modified: 'M',
    deleted: 'D',
    renamed: 'R',
    untracked: '?',
  };

  return (
    <div>
      <div
        className={`
          flex items-center gap-1 px-1 py-0.5 cursor-pointer rounded
          hover:bg-[#1a1a1a]
          ${onFileClick && !node.is_dir ? 'cursor-pointer' : 'cursor-default'}
        `}
        style={{ paddingLeft: `${depth * 12 + 4}px` }}
        onClick={() => {
          if (node.is_dir) {
            setExpanded(!expanded);
          } else if (onFileClick) {
            onFileClick(node.path);
          }
        }}
      >
        {node.is_dir ? (
          <span className="text-[#888] w-3 text-center">
            {expanded ? '▼' : '▶'}
          </span>
        ) : (
          <span className="w-3" />
        )}
        <span className={node.is_dir ? 'text-[#e0e0e0]' : 'text-[#aaa]'}>
          {node.is_dir ? '📁' : '📄'}
        </span>
        <span className={`flex-1 ${node.is_dir ? 'text-[#e0e0e0]' : 'text-[#aaa]'}`}>
          {node.name}
        </span>
        {status && (
          <span className={`${statusColors[status]} font-bold`}>
            {statusIcons[status]}
          </span>
        )}
      </div>
      {node.is_dir && expanded && node.children.map(child => (
        <TreeNode
          key={child.path}
          node={child}
          gitStatusMap={gitStatusMap}
          depth={depth + 1}
          onFileClick={onFileClick}
        />
      ))}
    </div>
  );
}
