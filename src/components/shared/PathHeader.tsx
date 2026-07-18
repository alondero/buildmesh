/**
 * Header strip showing the active directory path + a one-click jump to
 * the OS file manager. Shared between the Probe Panel's Files and Changes
 * tabs so the chrome (mono-font path, folder-open button) lives in one
 * place — WorktreeManagerTab (per-row copies) and GridNodeHeader
 * (agent node title bar) consume the extracted `FolderOpenIcon` for
 * the glyph itself, then wrap it in their own button semantics.
 */
import { openInFileManager } from '../../lib/tauri';
import { FolderOpenIcon } from './FolderOpenIcon';

interface PathHeaderProps {
  path: string;
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
