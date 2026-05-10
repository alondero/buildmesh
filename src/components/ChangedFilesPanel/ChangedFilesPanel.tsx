import { useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { getGitStatus, diffFileAgainstHead, type GitStatus, type DiffResult } from '../../lib/tauri';
import { GIT_CHANGED } from '../../lib/events';

interface ChangedFilesPanelProps {
  projectPath: string;
  isOpen: boolean;
  onFileSelect: (file: GitStatus, diff: DiffResult) => void;
}

export function ChangedFilesPanel({ projectPath, isOpen, onFileSelect }: ChangedFilesPanelProps) {
  const [files, setFiles] = useState<GitStatus[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [diffLoading, setDiffLoading] = useState<string | null>(null);

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

  useEffect(() => {
    if (!isOpen || !projectPath) return;
    const unlisten = listen<{ path: string }>(GIT_CHANGED, (event) => {
      if (event.payload.path === projectPath) {
        fetchStatus();
      }
    });
    return () => { unlisten.then(fn => fn()); };
  }, [isOpen, projectPath]);

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
    <div className="w-[280px] bg-bg-surface border-l border-border-subtle flex flex-col h-full overflow-hidden">
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
  );
}
