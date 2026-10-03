import { formatError } from '../../lib/errorUtils';
import { fileDiffStatusMeta } from '../../lib/status';
import type { GitStatus } from '../../lib/tauri';
import { useAgentChangedFiles } from '../../hooks/useAgentChangedFiles';
import { LoadingState } from '../shared/Spinner';

interface AgentChangedFilesListProps {
  nodeId: number;
  rootPath: string;
  selectedFile: string | null;
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
  selectedFile,
  onOpenFile,
}: AgentChangedFilesListProps) {
  const { files, loading, error, refresh } = useAgentChangedFiles(nodeId, rootPath);

  if (loading && files.length === 0) {
    return (
      <div className="flex-1 min-h-0 min-w-0 overflow-y-auto overflow-x-hidden">
        <LoadingState label="Loading changed files…" />
      </div>
    );
  }

  const additions = files.reduce((total, file) => total + file.additions, 0);
  const deletions = files.reduce((total, file) => total + file.deletions, 0);

  return (
    <div className="flex h-full min-h-0 min-w-0 flex-col">
      {error && <div className="shrink-0 border-b border-border-subtle p-3">
        <div role="alert" className="max-h-24 overflow-y-auto overflow-x-hidden break-all text-xs text-status-error">
          {formatError(error)}{files.length > 0 && ' — Showing last known changes.'}
        </div>
        <button type="button" disabled={loading} onClick={refresh} className="mt-2 min-h-[24px] rounded-md border border-border-default px-2 text-xs text-text-primary hover:bg-bg-card-hover">Retry changes</button>
      </div>}
      <div className="flex-1 min-h-0 min-w-0 overflow-y-auto overflow-x-hidden">
      {files.length > 0 && (
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
      )}

      {files.length === 0 ? (
        <div className="flex items-center justify-center h-40 text-text-muted text-xs">
          {error ? 'Changes unavailable' : 'No changes vs Base Ref'}
        </div>
      ) : (
        <div>
          {files.map((file) => (
            <ChangedFileRow
              key={file.path}
              file={file}
              selectedFile={selectedFile}
              onOpenFile={onOpenFile}
            />
          ))}
        </div>
      )}
      </div>
    </div>
  );
}

function ChangedFileRow({
  file,
  selectedFile,
  onOpenFile,
}: {
  file: GitStatus;
  selectedFile: string | null;
  onOpenFile: (path: string) => void;
}) {
  const meta = fileDiffStatusMeta(file.status);

  return (
    <button
      type="button"
      onClick={() => onOpenFile(file.path)}
      aria-label={`Open ${file.path} (${meta.label}, +${file.additions}, -${file.deletions}) in the center diff overlay`}
      title={file.path}
      className={`w-full flex items-center gap-2 px-3 py-2 text-xs font-mono text-left border-b border-border-subtle hover:bg-bg-card transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-accent-cyan ${
        selectedFile === file.path ? 'bg-bg-overlay' : ''
      }`}
    >
      <span className={`font-bold w-3 flex-shrink-0 ${meta.color}`} title={meta.label}>
        {meta.letter}
      </span>
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
