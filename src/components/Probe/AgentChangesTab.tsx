/**
 * AgentChangesTab — issue #376. The Probe Panel's 🔍 tab body.
 *
 * Wraps the existing `AgentReviewPanel` (ADR 0005 — stacked-diff review
 * surface) with the focused agent node's id and resolved path. By the time
 * this component mounts, `ProbeTabBody` has already guaranteed
 * `activeNodeId` is non-null (otherwise it would have shown the
 * "no active agent node" empty state), so the assertion below never
 * fires at runtime.
 *
 * The review surface itself is unchanged from PR #170 — every file the
 * agent changed since branching, sticky summary bar, jump-to-file index
 * built from the per-file sticky headers, and a collapsible FileTree for
 * opening any (even unchanged) file in the editor. This component adds the
 * binding to the Probe context, plus (issue #379) the per-file handoff to
 * the Center Workspace Diff Overlay.
 *
 * The overlay's diff baseline is `'base'` here — Agent Changes reviews
 * everything the node changed since branching (merge-base, ADR 0005), so the
 * overlay shows the same "vs base" view via `diff_node_file_against_base`.
 */

import { AgentReviewPanel } from '../FileTree/AgentReviewPanel';
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

  // Clicking a file's "open in center" button pops it into the spacious
  // overlay. We capture the focused node/mesh as the lens so the overlay
  // auto-closes if the user later focuses a different node or project.
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
      <AgentReviewPanel
        nodeId={activeNodeId}
        rootPath={activePath}
        onOpenFile={handleOpenFile}
      />
    </div>
  );
}
