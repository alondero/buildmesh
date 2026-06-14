/**
 * ProjectFilesTab — issue #376. The Probe Panel's 📁 tab body.
 *
 * Mirrors the non-agent branch of the legacy `FileExplorerPanel`: a
 * `ChangedFilesSection` on top, a collapsible `FileTree` underneath. The
 * tab is mesh-scoped — the active path comes from `useProbeContext()`, which
 * prefers the focused node's path (worktree dir) when a node is active and
 * falls back to the mesh root otherwise.
 *
 * Click semantics
 * ---------------
 * A click on a changed file switches the probe to the `review` tab via
 * `openDiff(file)` — the review tab is where the diff is consumed, not
 * inside the narrow 360px body of the files tab. A click on an unchanged
 * file opens it in the OS editor (the same default the legacy
 * `FileExplorerPanel` used).
 *
 * Why a separate component (not just importing `FileExplorerPanel`)
 * ---------------------------------------------------------------
 * `FileExplorerPanel` is a single component that branches on a
 * `context: { type: 'agent' | 'mesh' | 'userConfig' }` discriminator, with
 * its own resize handle, header, and close button — all of which are
 * unnecessary inside the Probe, where the dock already supplies the
 * header and the body width is fixed by `PROBE_BODY_WIDTH`. Lifting the
 * two child sections into a small dedicated component keeps the Probe
 * decoupled from the legacy panel's state machine.
 */

import { useState } from 'react';
import { openInEditor } from '../../lib/tauri';
import { FileTree } from '../FileTree/FileTree';
import { ChangedFilesSection } from '../FileTree/ChangedFilesSection';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useUIStore } from '../../stores/uiStore';

export function ProjectFilesTab() {
  const { activePath } = useProbeContext();
  const openDiff = useUIStore((s) => s.openDiff);

  const [fileTreeExpanded, setFileTreeExpanded] = useState(true);

  // ProbeTabBody guarantees `activePath` is non-null by the time this
  // component renders (it shows the "no project selected" empty state
  // otherwise). The non-null assertion keeps the child components' types
  // narrow without a runtime check that would never fire.
  if (!activePath) return null;

  // Wire both the changed-files list and the file-tree's changed-file
  // rows through `openDiff` so the user lands on the review tab — the
  // probe's 360px body can't host an inline diff, so this is the natural
  // handoff. The `DiffResult` argument is ignored: `openDiff` only needs
  // the file path to populate the review tab's `activeDiffFile`. The
  // `string | null` signature is dictated by `FileTree.onFileSelect`'s
  // "clear the selection" hook — we treat null as a no-op.
  const handleChangedFileSelect = (path: string | null) => {
    if (path) openDiff(path);
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
      <ChangedFilesSection
        rootPath={activePath}
        selectedFile={null}
        onChangedFileSelect={handleChangedFileSelect}
      />
      <div className="border-b border-border-subtle">
        <button
          onClick={() => setFileTreeExpanded(!fileTreeExpanded)}
          className="w-full flex items-center gap-1 px-2 py-1.5 text-[11px] font-medium text-text-secondary hover:bg-bg-card transition-colors"
        >
          <span className="text-text-muted w-3 text-center text-[10px]">
            {fileTreeExpanded ? '▼' : '▶'}
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
