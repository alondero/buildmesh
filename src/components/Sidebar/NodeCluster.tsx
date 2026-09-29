import { memo } from 'react';
import type { SpawnOption } from '../../lib/groups';
import type { NodeActivityCluster } from '../../lib/nodeActivities';
import { activityMemberRole, activityStatus } from '../../lib/nodeActivities';
import type { getMeshColor } from '../../lib/meshColors';
import { NodeItem } from './NodeItem';

type MeshColor = ReturnType<typeof getMeshColor>;

interface NodeClusterProps {
  cluster: NodeActivityCluster;
  meshColor: MeshColor;
  providerList: SpawnOption[];
  onSelectNode: (nodeId: number, meshId: number) => void;
  onDeleteNode: (e: React.MouseEvent, nodeId: number) => void;
}

/// Indent of the sub-member rail from the sidebar's left edge. The header
/// marker and the rail share it so the cluster reads as one connected tree
/// rather than a disconnected step.
const RAIL_INDENT = 'ml-2';

/** One Node Activity drawn in the mesh sidebar.
 *
 *  A cluster of one renders exactly the row the flat list used to render, with
 *  no rail, no indent, and no extra wrapper — the overwhelmingly common case
 *  (a mesh with no pairings) is unchanged. Only a genuinely paired cluster pays
 *  for the chrome.
 *
 *  The root row keeps its full indent and the remaining members step in under a
 *  vertical rail, so the eye reads one card with sub-agents rather than N
 *  unrelated rows. Each member is still its own `NodeItem`: click, rename,
 *  status, context menu, and delete all keep working per node, because a paired
 *  member remains an independent Agent Node with its own worktree and lifecycle
 *  (CONTEXT.md). */
function NodeClusterView({ cluster, meshColor, providerList, onSelectNode, onDeleteNode }: NodeClusterProps) {
  const { root, members, paired, handGrouped } = cluster;
  if (!paired) {
    return <NodeItem node={root} meshColor={meshColor} providerList={providerList}
      onSelectNode={onSelectNode} onDeleteNode={onDeleteNode} />;
  }
  const status = activityStatus(root, members, true);
  // `clusterActivityNodes` normalises `members` to representative-first, but
  // filter by identity rather than slicing `[1]` so a future caller that hands
  // us a differently-ordered array can neither drop a member nor duplicate the
  // header. Cheap enough to be unconditional.
  const rest = members.filter(member => member.id !== root.id);
  // Hovering the marker names every member with its role, so the pairing stays
  // legible when the sub-rows are scrolled out of a short sidebar.
  const roster = members
    .map((member, index) => `${activityMemberRole(member, root.id, members.length, index - 1, handGrouped)}: ${member.name}`)
    .join('\n');
  return (
    <div data-node-cluster-id={root.id} data-paired="true" className="mb-0.5">
      {/* Both the header marker and the sub-member rail sit at `ml-2`, so the
          marker's edge lands exactly on the rail and the cluster reads as one
          connected tree instead of a disconnected step. */}
      <div className={`relative ${RAIL_INDENT}`}>
        {/* Paired marker on the header row. `role="img"` with an explicit label
            carries the meaning to assistive tech, which a bare glyph would not.
            Its colour tracks the group's COMBINED tone — a crashed reviewer
            repaints it red even while the implementer's own row looks healthy.
            Tone→token mapping mirrors `GridNodeHeader`'s activity dot, per
            DESIGN.md rule 3 ("colour means status"). */}
        <span
          data-cluster-marker
          role="img"
          aria-label={`Paired group of ${members.length} agents — ${status.label}`}
          title={`Paired with ${members.length - 1} other agent${members.length === 2 ? '' : 's'} — ${status.label}\n\n${roster}`}
          className={`absolute left-0 top-0 h-full w-0.5 rounded-full ${
            status.tone === 'error' ? 'bg-status-error'
              : status.tone === 'warning' ? 'bg-status-warning'
                : status.tone === 'active' ? 'bg-accent-cyan'
                  : 'bg-accent-cyan/40'}`}
        />
        <NodeItem node={root} meshColor={meshColor} providerList={providerList}
          onSelectNode={onSelectNode} onDeleteNode={onDeleteNode} />
      </div>
      <ul className={`relative list-none border-l-2 border-border-subtle pl-0 ${RAIL_INDENT}`} aria-label="Paired agents">
        {rest.map(member => (
          <li key={member.id} className="relative pl-2">
            {/* Elbow joining this member to the rail. */}
            <span aria-hidden="true" className="absolute -left-px top-1/2 h-px w-2 bg-border-subtle" />
            <NodeItem node={member} meshColor={meshColor} providerList={providerList}
              onSelectNode={onSelectNode} onDeleteNode={onDeleteNode} />
          </li>
        ))}
      </ul>
    </div>
  );
}

/// `Sidebar` rebuilds the cluster objects on every node update, so comparing the
/// cluster by identity would never bail — the same trap `sameMeshNodeRefs`
/// avoids on `MeshItem`'s `meshNodes`. Compare element-wise instead: the store's
/// shallow reconciliation (issue #1384) preserves per-node references for
/// untouched nodes, so identical member references mean "this cluster's data is
/// unchanged" and the memoized `NodeItem` rows below are left alone.
function sameClusterRefs(previous: NodeActivityCluster, next: NodeActivityCluster): boolean {
  return previous.root === next.root
    && previous.paired === next.paired
    && previous.handGrouped === next.handGrouped
    && previous.members.length === next.members.length
    && previous.members.every((node, index) => node === next.members[index]);
}

export const NodeCluster = memo(NodeClusterView, (previous, next) =>
  sameClusterRefs(previous.cluster, next.cluster)
  && previous.meshColor === next.meshColor
  && previous.providerList === next.providerList
  && previous.onSelectNode === next.onSelectNode
  && previous.onDeleteNode === next.onDeleteNode);
