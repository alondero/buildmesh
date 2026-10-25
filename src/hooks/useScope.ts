/**
 * The read seam for "what is the user looking at" (issue #2529).
 *
 * View Mode, the selected Mesh, and the active Agent Node stay in their own
 * stores. This hook is the only place that joins them into the effective
 * scope. It delegates to `deriveScope` and `resolveSingleNode` and does not
 * restate either rule. `readScope` is the same assembly for imperative
 * readers (keyboard traversal) that cannot call a hook.
 *
 * Mesh names are opt-in. The title bar asks for them; the canvas does not,
 * because subscribing the grid to the Mesh list re-renders every terminal
 * on a rename.
 *
 * Card selections stay out of this snapshot. A utility toggle is per card,
 * and folding `nodeActivityStore.selections` into every scope consumer would
 * re-render the grid for a change one card already subscribes to. The member
 * rule itself lives here as `selectedActivityMember`, so the card does not
 * own a second chain. `diffLensDiverged` is the diff overlay's close rule,
 * moved here for the same reason: it compares the raw focus and Mesh
 * selection, which are not the derived grid Mesh.
 */

import { useMemo } from 'react';
import { useMeshStore, type Mesh } from '../stores/meshStore';
import { useAgentNodeStore, useAllAgentNodes, type AgentNode } from '../stores/agentNodeStore';
import { useUIStore } from '../stores/uiStore';
import {
  deriveScope,
  resolveSingleNode,
  type DerivedScope,
  type FilterControls,
  type ScopeInput,
} from '../lib/viewModes';

const NO_MESH_NAMES: readonly Mesh[] = [];

export interface Scope extends DerivedScope {
  /** Anchoring Mesh id. Null when the grid is cross-Mesh or Mesh Grid has no selection. */
  scopeMeshId: number | null;
  /** Nodes the scope shows right now. The same array as `visibleNodes`. */
  scopedNodes: readonly AgentNode[];
  /**
   * The node Single mode solos, from `resolveSingleNode`. Null in every grid
   * mode, where the canvas shows `scopedNodes` rather than one node.
   */
  currentNodeId: number | null;
  currentNode: AgentNode | null;
  /** Sidebar selection, including a Mesh that is not the grid's anchor. */
  selectedMeshId: number | null;
  /** Focused Agent Node, before the Single-mode fallback. */
  activeNodeId: number | null;
}

export interface ScopeReadOptions {
  /** Resolve anchoring-Mesh display names. Leave this off on the canvas. */
  meshNames?: boolean;
}

function assembleScope(input: ScopeInput): Scope {
  const derived = deriveScope(input);
  const currentNode = input.viewMode === 'single'
    ? resolveSingleNode(
      input.agentNodes,
      input.activeNodeId,
      input.lastNonSingleMode,
      input.selectedMeshId,
      input.controls,
    )
    : null;
  return {
    ...derived,
    scopeMeshId: derived.mesh?.id ?? null,
    scopedNodes: derived.visibleNodes,
    currentNode,
    currentNodeId: currentNode?.id ?? null,
    selectedMeshId: input.selectedMeshId,
    activeNodeId: input.activeNodeId,
  };
}

function controlsFrom(ui: {
  gridSearchQuery: string;
  gridProviderFilter: FilterControls['gridProviderFilter'];
  gridStatusFilter: FilterControls['gridStatusFilter'];
}): FilterControls {
  return {
    gridSearchQuery: ui.gridSearchQuery,
    gridProviderFilter: ui.gridProviderFilter,
    gridStatusFilter: ui.gridStatusFilter,
  };
}

export function readScope(options?: ScopeReadOptions): Scope {
  const ui = useUIStore.getState();
  const mesh = useMeshStore.getState();
  const nodes = useAgentNodeStore.getState();
  return assembleScope({
    viewMode: ui.viewMode,
    lastNonSingleMode: ui.lastNonSingleMode,
    agentNodes: nodes.getAgentNodes(),
    selectedMeshId: mesh.selectedMeshId,
    activeNodeId: nodes.activeNodeId,
    meshes: options?.meshNames ? mesh.meshes : undefined,
    controls: controlsFrom(ui),
  });
}

export function useScope(options?: ScopeReadOptions): Scope {
  const meshNames = options?.meshNames ?? false;
  const selectedMeshId = useMeshStore((s) => s.selectedMeshId);
  const meshes = useMeshStore((s) => (meshNames ? s.meshes : NO_MESH_NAMES));
  const activeNodeId = useAgentNodeStore((s) => s.activeNodeId);
  const agentNodes = useAllAgentNodes();
  const viewMode = useUIStore((s) => s.viewMode);
  const lastNonSingleMode = useUIStore((s) => s.lastNonSingleMode);
  const gridSearchQuery = useUIStore((s) => s.gridSearchQuery);
  const gridProviderFilter = useUIStore((s) => s.gridProviderFilter);
  const gridStatusFilter = useUIStore((s) => s.gridStatusFilter);

  return useMemo(
    () => assembleScope({
      viewMode,
      lastNonSingleMode,
      agentNodes,
      selectedMeshId,
      activeNodeId,
      meshes: meshNames ? meshes : undefined,
      controls: { gridSearchQuery, gridProviderFilter, gridStatusFilter },
    }),
    [
      viewMode,
      lastNonSingleMode,
      agentNodes,
      selectedMeshId,
      activeNodeId,
      meshNames,
      meshes,
      gridSearchQuery,
      gridProviderFilter,
      gridStatusFilter,
    ],
  );
}

/**
 * Close a diff when focus or the selected Mesh leaves the lens it was opened
 * on. A null captured node (every pull-request diff) does not count as a node
 * change, and a cleared Mesh selection does not count as a Mesh change.
 * Selecting the diff's own Mesh is not a divergence.
 */
export function diffLensDiverged(
  scope: Pick<Scope, 'activeNodeId' | 'selectedMeshId'>,
  captured: { nodeId: number | null; meshId: number },
): boolean {
  const nodeChanged = captured.nodeId !== null && scope.activeNodeId !== captured.nodeId;
  const meshChanged = scope.selectedMeshId !== null && scope.selectedMeshId !== captured.meshId;
  return nodeChanged || meshChanged;
}

/**
 * Which member a Node Activity card shows. The active member wins, then the
 * card's own selection, then the card root. Selections that name a node
 * outside the card do not count.
 */
export function selectedActivityMember(
  members: readonly { id: number }[],
  activeNodeId: number | null,
  selectionNodeId: number | null | undefined,
  rootId: number,
): number {
  return members.find((member) => member.id === activeNodeId)?.id
    ?? members.find((member) => member.id === selectionNodeId)?.id
    ?? rootId;
}
