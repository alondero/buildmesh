/**
 * AgentChangesTab — issue #376. The Probe Panel's Agent Changes tab body.
 *
 * Shows the focused Agent Node's lightweight base-relative changed-file list.
 * By the time this component mounts, `ProbeTabBody` has already guaranteed
 * `activeNodeId` is non-null (otherwise it would have shown the "no active
 * agent node" empty state), so the assertion below never fires at runtime.
 *
 * The list uses `node_changed_files`, which returns paths and line counts
 * without constructing or highlighting every hunk. A click hands one path
 * to the Center Workspace Diff Overlay, which then loads only that file.
 *
 * The overlay's diff baseline is `'base'` here — Agent Changes reviews
 * everything the node changed since branching (merge-base, ADR 0005), so the
 * overlay shows the same "vs base" view via `diff_node_file_against_base`.
 */

import { AgentChangedFilesList } from './AgentChangedFilesList';
import { PathHeader } from '../shared/PathHeader';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useUIStore } from '../../stores/uiStore';

export function AgentChangesTab() {
  const { activeNodeId, activePath, activeMeshId } = useProbeContext();
  const openDiff = useUIStore((s) => s.openDiff);

  // ProbeTabBody gates on `activeNodeId !== null` (and a selected mesh) before
  // mounting this component, so the guard is a type-narrowing convenience
  // rather than a runtime guard.
  if (activeNodeId === null || activePath === null || activeMeshId === null) return null;

  // Clicking a file opens its single-file diff in the spacious overlay. We
  // capture the focused node/mesh as the lens so the overlay auto-closes if
  // the user later focuses a different node or project.
  const handleOpenFile = (filePath: string) =>
    openDiff({
      filePath,
      rootPath: activePath,
      nodeId: activeNodeId,
      meshId: activeMeshId,
      source: 'base',
    });

  // PathHeader must sit outside the scroll context so it stays pinned
  // at the top while diffs scroll underneath.
  return (
    <div className="flex flex-col h-full overflow-hidden">
      <PathHeader path={activePath} />
      <AgentChangedFilesList
        nodeId={activeNodeId}
        rootPath={activePath}
        onOpenFile={handleOpenFile}
      />
    </div>
  );
}
