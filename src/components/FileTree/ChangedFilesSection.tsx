import { useState, useEffect } from 'react';
import {
  getGitStatus,
  diffFileAgainstHead,
  type GitStatus,
  type DiffResult,
} from '../../lib/tauri';

interface ChangedFilesSectionProps {
  /** Repo root whose uncommitted changes we list. */
  rootPath: string;
  /** Currently open diff path, to highlight the active row. */
  selectedFile: string | null;
  /** Called with the file's relative path and freshly-loaded diff. */
  onChangedFileSelect: (path: string, diff: DiffResult) => void;
}

const statusColors: Record<string, string> = {
  added: 'text-accent-green',
  modified: 'text-accent-amber',
  deleted: 'text-accent-red',
  renamed: 'text-purple-400',
  untracked: 'text-text-muted',
};

const statusPrefix: Record<string, string> = {
  added: 'A',
  modified: 'M',
  deleted: 'D',
  renamed: 'R',
  untracked: '?',
};

export function ChangedFilesSection({
  rootPath,
  selectedFile,
  onChangedFileSelect,
}: ChangedFilesSectionProps) {
  const [files, setFiles] = useState<GitStatus[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [expanded, setExpanded] = useState(true);

  useEffect(() => {
    if (!rootPath) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    getGitStatus(rootPath)
      .then((status) => {
        if (cancelled) return;
        setFiles(status);
        setLoading(false);
      })
      .catch((e) => {
        if (cancelled) return;
        setError(String(e));
        setLoading(false);
      });
    return () => { cancelled = true; };
  }, [rootPath]);

  // get_git_status returns paths relative to the repo root, which is exactly
  // what diff_file_against_head expects — no conversion needed.
  const handleClick = async (path: string) => {
    try {
      const diff = await diffFileAgainstHead(rootPath, path);
      onChangedFileSelect(path, diff);
    } catch (e) {
      console.error('Failed to load diff:', e);
    }
  };

  return (
    <div className="border-b border-border-subtle">
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full flex items-center gap-1 px-2 py-1.5 text-[11px] font-medium text-text-secondary hover:bg-bg-card transition-colors"
      >
        <span className="text-text-muted w-3 text-center text-[10px]">
          {expanded ? '▼' : '▶'}
        </span>
        <span className="flex-1 text-left">Changed Files</span>
        <span className="text-text-muted text-[10px]">{files.length}</span>
      </button>
      {expanded && (
        <div className="pb-1">
          {loading ? (
            <div className="px-3 py-1.5 text-text-muted text-xs">Loading…</div>
          ) : error ? (
            <div className="px-3 py-1.5 text-accent-red text-xs">Error: {error}</div>
          ) : files.length === 0 ? (
            <div className="px-3 py-1.5 text-text-muted text-xs">No changes</div>
          ) : (
            files.map((file) => (
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
                <span className={`font-bold w-3 flex-shrink-0 ${statusColors[file.status] ?? 'text-text-muted'}`}>
                  {statusPrefix[file.status] ?? '?'}
                </span>
                <span className="flex-1 truncate text-text-muted">{file.path}</span>
                <span className="text-accent-green flex-shrink-0">+{file.additions}</span>
                <span className="text-accent-red flex-shrink-0">-{file.deletions}</span>
              </button>
            ))
          )}
        </div>
      )}
    </div>
  );
}
