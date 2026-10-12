import { useAgentNodeStore } from '../stores/agentNodeStore';
import { useMeshStore } from '../stores/meshStore';
import { useNodeActivityStore, type UtilityMode } from '../stores/nodeActivityStore';
import { useUIStore, type ProbeTab } from '../stores/uiStore';

interface NodeFocusOptions {
  /** Current keeps the grid scope; mesh follows a node without leaving Single. */
  view?: 'current' | 'mesh' | 'single';
  /** Current retains the grid behind a solo card; node follows a newly created node. */
  scope?: 'current' | 'node';
  probeTab?: ProbeTab;
  utility?: boolean;
  utilityMode?: UtilityMode;
}

/** Returns false when a stale target can no longer be displayed. */
export function focusNode(nodeId: number, options: NodeFocusOptions = {}): boolean {
  const node = useAgentNodeStore.getState().nodesById[nodeId];
  if (!node || node.status === 'archived') return false;

  const ui = useUIStore.getState();
  if (options.view === 'mesh' && (ui.viewMode !== 'single' || options.scope === 'node')
    && useMeshStore.getState().selectedMeshId !== node.mesh_id) {
    enterMesh(node.mesh_id);
  } else if (options.view === 'single') {
    if (options.scope !== 'current' && useMeshStore.getState().selectedMeshId !== node.mesh_id) {
      enterMesh(node.mesh_id);
    }
    ui.setViewMode('single');
  }
  useNodeActivityStore.getState().activateNode(nodeId, options.utility, options.utilityMode);
  if (options.probeTab) ui.openProbeTab(options.probeTab);
  return true;
}

/** Re-entering the selected mesh restores its grid, including from Single. */
export function enterMesh(meshId: number, options: { probeTab?: ProbeTab; nodeId?: number } = {}): boolean {
  if (options.nodeId !== undefined) {
    const node = useAgentNodeStore.getState().nodesById[options.nodeId];
    if (!node || node.status === 'archived' || node.mesh_id !== meshId) return false;
    focusNode(options.nodeId);
  }
  if (useMeshStore.getState().selectedMeshId !== meshId) {
    useMeshStore.getState().selectMesh(meshId);
  }
  const ui = useUIStore.getState();
  ui.setViewMode('mesh');
  if (options.probeTab) ui.openProbeTab(options.probeTab);
  return true;
}

/** Scope the Circuits snapshot before requesting a run within it. */
export function revealRun(meshId: number, runId: number): void {
  enterMesh(meshId);
  useUIStore.getState().focusCircuitRun(runId);
}
