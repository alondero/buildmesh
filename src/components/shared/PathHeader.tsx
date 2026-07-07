/**
 * PathHeader — header strip showing a directory path with an "open in
 * file explorer" affordance.
 *
 * Used by the Probe Panel's Files and Changes tabs to surface the focused
 * agent node's worktree path (or the mesh root with no node focused) and
 * give the user a one-click jump into the OS file manager.
 *
 * DOM contract (pinned by tests/unit/project-files-tab.test.tsx and
 * tests/unit/path-header.test.tsx):
 *   - The path text uses a mono font and `truncate` with a `title` attribute
 *     so the full path is reachable via hover tooltip.
 *   - The action is a real `<button>` with aria-label="Open in file explorer"
 *     and contains an `<svg>` glyph — pin the SVG so the glyph can't be
 *     silently replaced with text/emoji.
 *   - Clicking fires `open_in_file_manager` via the Tauri IPC seam with
 *     `path` as the directory.
 */
import { openInFileManager } from '../../lib/tauri';

interface PathHeaderProps {
  /** Directory to display and to pass to `open_in_file_manager`. */
  path: string;
}

/** Lucide folder-open. */
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