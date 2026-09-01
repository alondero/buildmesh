/**
 * Resolve the context owned by the active Probe destination (issue #1456).
 *
 * The stores remain the sources of truth for Meshes, Agent Nodes, and canvas
 * selection. This hook is the read seam that combines them with the
 * destination's explicit ownership definition and an optional destination-
 * local pin. It owns no state of its own, so every consumer sees the same
 * answer and a pin cannot drift from the stores silently.
 */

import { useMemo } from 'react';
import { useMeshStore } from '../stores/meshStore';
import { useAgentNodeStore } from '../stores/agentNodeStore';
import { useUIStore } from '../stores/uiStore';
import {
  resolveProbeContext,
  type ProbeContext,
} from '../lib/probeContext';

export type {
  ProbeBaseline,
  ProbeContext,
  ProbeContextMesh,
  ProbeContextMode,
  ProbeContextNode,
  ProbeContextPin,
  ProbeLens,
  ProbeSubject,
  ProbeTabDefinition,
} from '../lib/probeContext';
export {
  PROBE_TAB_DEFINITIONS,
  PROBE_TAB_ORDER,
  formatProbeSubject,
  resolveProbeContext,
} from '../lib/probeContext';

export function useProbeContext(): ProbeContext {
  const selectedMeshId = useMeshStore((s) => s.selectedMeshId);
  const meshesById = useMeshStore((s) => s.meshesById);
  const activeNodeId = useAgentNodeStore((s) => s.activeNodeId);
  // Subscribe to the normalized map directly. The active node is the only
  // entity this hook needs; unrelated status changes should not fan out into
  // every Probe consumer (issue #1384).
  const nodesById = useAgentNodeStore((s) => s.nodesById);
  const viewMode = useUIStore((s) => s.viewMode);
  const probeTab = useUIStore((s) => s.probeTab);
  // Resolve only the active destination's entry. A pin on another tab should
  // not cause this destination to re-render or borrow that tab's subject.
  const probeContextPin = useUIStore((s) => s.probeContextPins[s.probeTab] ?? null);

  return useMemo(
    () => resolveProbeContext({
      tab: probeTab,
      selectedMeshId,
      meshesById,
      activeNodeId,
      nodesById,
      viewMode,
      pin: probeContextPin,
    }),
    [
      selectedMeshId,
      meshesById,
      activeNodeId,
      nodesById,
      viewMode,
      probeTab,
      probeContextPin,
    ],
  );
}
