/**
 * ProjectFilesTab — issue #376. The Probe Panel's Project Files tab body.
 *
 * Mirrors the non-agent branch of the legacy `FileExplorerPanel`: a
 * `ChangedFilesSection` on top, a collapsible `FileTree` underneath. The
 * tab is Mesh-scoped — the active path comes from `useProbeContext()`, which
 * uses the focused/pinned Agent Node's working tree only when it belongs to
 * the destination Mesh and falls back to the Mesh root otherwise.
 *
 * Click semantics
 * ---------------
 * A click on a changed file opens its diff in the Center Workspace Diff
 * Overlay (issue #379) via `openDiff(ctx)` — the spacious center surface is
 * where the diff is consumed, not inside the narrow 360px body of the files
 * tab. The Probe stays on this tab so the list keeps responding to clicks. A
 * click on an unchanged file opens it in the OS editor (the same default the
 * legacy `FileExplorerPanel` used).
 *
 * The overlay's diff baseline is `'head'` here — Project Files lists
 * uncommitted working-tree changes (`get_git_status` / `diff_file_against_head`),
 * so the overlay shows the same "vs HEAD" view the user clicked.
 *
 * Why a separate component (not just importing `FileExplorerPanel`)
 * ---------------------------------------------------------------
 * `FileExplorerPanel` is a single component that branches on a
 * `context: { type: 'agent' | 'mesh' | 'userConfig' }` discriminator, with
 * its own resize handle, header, and close button — all of which are
 * unnecessary inside the Probe, where the dock already supplies the
 * header and the body width is driven by `useProbeResize` (issue #724,
 * 240-720px clamp, localStorage-persisted). Lifting the two child
 * sections into a small dedicated component keeps the Probe decoupled
 * from the legacy panel's state machine.
 */

import { useState } from 'react';
import { openInEditor } from '../../lib/tauri';
import { FileTree } from '../FileTree/FileTree';
import { ChangedFilesSection } from '../FileTree/ChangedFilesSection';
import { PathHeader } from '../shared/PathHeader';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useUIStore } from '../../stores/uiStore';

/** Lucide chevron-right, rotated 90° when a row is expanded (used by the
 *  "File Tree" section header above and by the tree's own directory rows). */
function ChevronRightIcon({ className }: { className?: string }) {
  return (
    <svg
      className={className}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth="2"
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden
    >
      <polyline points="9 18 15 12 9 6" />
    </svg>
  );
}

export function ProjectFilesTab() {
  const { activePath, activeMeshId, activeNodeId } = useProbeContext();
  const openDiff = useUIStore((s) => s.openDiff);

  const [fileTreeExpanded, setFileTreeExpanded] = useState(true);

  // ProbeTabBody guarantees `activePath` and `activeMeshId` are non-null by the
  // time this component renders (it shows the explicit missing-Mesh empty state
  // otherwise). The guard keeps the context's types narrow without a runtime
  // check that would normally fire.
  if (!activePath || activeMeshId === null) return null;

  // A changed-file click opens the Center Workspace Diff Overlay (#379). The
  // diff is `'head'`-source (uncommitted vs HEAD) because that's what the
  // Changed Files list shows; `rootPath` is the probe's active path (the
    // focused/pinned node's worktree, or the mesh root with no matching node), so the
  // path joins correctly for `diff_file_against_head`. We capture the focused
  // node/mesh as the lens so the overlay auto-closes if it later changes. The
  // `string | null` signature is dictated by `FileTree.onFileSelect`'s "clear
  // the selection" hook — we treat null as a no-op.
  const handleChangedFileSelect = (path: string | null) => {
    if (!path) return;
    openDiff({
      filePath: path,
      rootPath: activePath,
      nodeId: activeNodeId,
      meshId: activeMeshId,
      source: 'head',
    });
  };
  const handleUnchangedFileSelect = async (path: string) => {
    try {
      await openInEditor(path);
    } catch (e) {
      console.error('Failed to open file in editor:', e);
    }
  };

  return (
    <div className="flex-1 overflow-auto">
      <PathHeader path={activePath} />
      <ChangedFilesSection
        rootPath={activePath}
        selectedFile={null}
        onChangedFileSelect={handleChangedFileSelect}
      />
      <div className="border-b border-border-subtle">
        <button
          onClick={() => setFileTreeExpanded(!fileTreeExpanded)}
          className="w-full flex items-center gap-1 px-2 py-1.5 text-xs font-medium text-text-secondary hover:bg-bg-card transition-colors"
        >
          <span
            className={`w-3 h-3 flex items-center justify-center text-text-muted transition-transform ${
              fileTreeExpanded ? 'rotate-90' : ''
            }`}
          >
            <ChevronRightIcon className="w-3 h-3" />
          </span>
          <span className="flex-1 text-left">File Tree</span>
        </button>
        {fileTreeExpanded && (
          <FileTree
            rootPath={activePath}
            showGitStatus
            selectedFile={null}
            // `FileTree` routes ALL clicks (changed + unchanged) through
            // `onFileSelect` for our use case — the probe's 360px body
            // can't host an inline diff, so a changed-file click should
            // also switch to the review tab. `onChangedFileSelect` is
            // intentionally omitted: the `FileTree` guards the call on
            // the prop being defined, so leaving it out routes every
            // click through `onFileSelect` without a no-op stub.
            onFileSelect={handleChangedFileSelect}
            onUnchangedFileSelect={handleUnchangedFileSelect}
          />
        )}
      </div>
    </div>
  );
}
