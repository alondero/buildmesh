import { formatError } from '../../lib/errorUtils';
import type { GitStatus } from '../../lib/tauri';
import { useAgentChangedFiles } from '../../hooks/useAgentChangedFiles';
import { LoadingState } from '../shared/Spinner';

interface AgentChangedFilesListProps {
  nodeId: number;
  rootPath: string;
  onOpenFile: (path: string) => void;
}

/**
 * Lightweight Agent Changes surface. The initial probe only asks Git for the
 * changed-file list and line counts; a full diff is fetched by the centre
 * overlay when the user clicks a row.
 */
export function AgentChangedFilesList({
  nodeId,
  rootPath,
  onOpenFile,
}: AgentChangedFilesListProps) {
  const { files, loading, error } = useAgentChangedFiles(nodeId, rootPath);

  if (loading && files.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center">
        <LoadingState label="Loading changed files…" />
      </div>
    );
  }

  if (error && files.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center text-accent-red text-xs px-3 text-center">
        {formatError(error)}
      </div>
    );
  }

  const additions = files.reduce((total, file) => total + file.additions, 0);
  const deletions = files.reduce((total, file) => total + file.deletions, 0);

  return (
    <div className="flex-1 min-h-0 overflow-auto">
      <div className="sticky top-0 z-10 flex items-center gap-2 px-3 py-1.5 bg-bg-overlay border-b border-border-subtle text-xs">
        <span className="text-text-secondary font-medium">
          {files.length} {files.length === 1 ? 'file' : 'files'} changed
        </span>
        {additions > 0 && (
          <span className="text-accent-green font-mono">+{additions}</span>
        )}
        {deletions > 0 && (
          <span className="text-accent-red font-mono">-{deletions}</span>
        )}
        <span
          className="ml-auto text-text-muted"
          title="Changes since this agent branched from its base"
        >
          vs base
        </span>
      </div>

      {error && files.length > 0 && (
        <div
          role="status"
          aria-live="polite"
          className="px-3 py-1.5 bg-status-warning/10 border-b border-status-warning/30 text-2xs text-status-warning"
        >
          Refresh failed — showing last known changes
        </div>
      )}

      {files.length === 0 ? (
        <div className="flex items-center justify-center h-40 text-text-muted text-xs">
          No changes vs Base Ref
        </div>
      ) : (
        <div>
          {files.map((file) => (
            <ChangedFileRow key={file.path} file={file} onOpenFile={onOpenFile} />
          ))}
        </div>
      )}
    </div>
  );
}

function ChangedFileRow({
  file,
  onOpenFile,
}: {
  file: GitStatus;
  onOpenFile: (path: string) => void;
}) {
  return (
    <button
      type="button"
      onClick={() => onOpenFile(file.path)}
      aria-label={`Open ${file.path} in the center diff overlay`}
      title={file.path}
      className="w-full flex items-center gap-2 px-3 py-2 text-xs font-mono text-left border-b border-border-subtle hover:bg-bg-card transition-colors"
    >
      <span className="flex-1 min-w-0 truncate text-text-primary">{file.path}</span>
      <span className="flex items-center gap-1.5 shrink-0">
        {file.additions > 0 && (
          <span className="text-accent-green">+{file.additions}</span>
        )}
        {file.deletions > 0 && (
          <span className="text-accent-red">-{file.deletions}</span>
        )}
      </span>
    </button>
  );
}
