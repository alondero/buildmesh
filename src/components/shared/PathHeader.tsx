/**
 * Header strip showing the active directory path + a one-click jump to
 * the OS file manager. Shared between the Probe Panel's Files and Changes
 * tabs so the chrome (mono-font path, folder-open button) lives in one
 * place — WorktreeManagerTab's per-row copies are a planned future
 * consumer (needs `aria-label` + `data-testid` extensions first).
 */
import { openInFileManager } from '../../lib/tauri';

interface PathHeaderProps {
  path: string;
}

function FolderOpenIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="1.75"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <path d="M6 14l1.45-2.9A2 2 0 0 1 9.24 10H20a2 2 0 0 1 1.94 2.5l-1.55 6a2 2 0 0 1-1.94 1.5H4a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h3.93a2 2 0 0 1 1.66.9l.82 1.2a2 2 0 0 0 1.66.9H18a2 2 0 0 1 2 2v2" />
    </svg>
  );
}

export function PathHeader({ path }: PathHeaderProps) {
  const handleOpenInFileManager = async () => {
    try {
      await openInFileManager(path);
    } catch (e) {
      console.error('Failed to open folder in file manager:', e);
    }
  };

  return (
    <div className="flex items-center justify-between px-2 py-1.5 border-b border-border-subtle">
      <span
        className="text-xs font-mono text-text-secondary truncate flex-1 min-w-0"
        title={path}
      >
        {path}
      </span>
      <button
        type="button"
        onClick={handleOpenInFileManager}
        aria-label="Open in file explorer"
        title="Open in file explorer"
        className="p-1 rounded-md text-text-muted hover:text-accent-cyan hover:bg-bg-card transition-colors flex-shrink-0 ml-1"
      >
        <FolderOpenIcon className="w-3.5 h-3.5" />
      </button>
    </div>
  );
}