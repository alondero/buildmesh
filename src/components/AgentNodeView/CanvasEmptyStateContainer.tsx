/**
 * Issue #1536 — encapsulation container for the canvas empty state.
 *
 * Owns every empty-state-only store subscription:
 *
 *   - `useProviderList()` — the shared module-scope provider cache that
 *     re-publishes on `provider-list-changed` events. Subscribing to
 *     it from the root `AgentNodeView` would cascade re-renders into
 *     every NodeCard / Terminal the user is actively typing into
 *     (senior-review round 4).
 *   - `useMeshStore.meshes.length` — drives the `no-meshes` classifier
 *     branch and the dropdown's `meshCount` field.
 *   - The five uiStore action bindings (`openCreateMesh`,
 *     `openCanvasSpawnMenu`, `resetGridControls`, `openAppSettings`,
 *     `setViewMode`) — these stable action references are fine to
 *     bind at any level, but isolating them here means the active
 *     canvas never sees them.
 *
 * Mounting strategy: `AgentNodeView` renders this component ONLY
 * when the canvas should show an empty state
 * (`viewMode === 'single' ? singleNode === null : visibleNodes.length === 0`).
 * When the canvas has content, the container unmounts and all its
 * subscriptions go with it — leaving the terminal grid free of
 * reactive pollution.
 *
 * The container still has all the props it strictly needs (the parent's
 * already-subscribed-to values — `viewMode`, `lastNonSingleMode`,
 * `selectedMeshId`, `activeNodeId`, `agentNodes`, `visibleNodes.length`)
 * so the input shape and classifier behavior stay identical to the
 * pre-encapsulation inline form. The container just owns the empty-
 * state-only subscriptions.
 *
 * The pure `CanvasEmptyState` component (this file's sibling) remains
 * trivially unit-testable — its contract is the input-shape-to-branch
 * mapping, no store subscriptions, no callbacks, no DOM. The container
 * is the integration glue that wires the input to live state.
 */
import { useMemo } from 'react';
import type { AgentNode } from '../../stores/agentNodeStore';
import type { NonSingleViewMode, ViewMode } from '../../stores/uiStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { scopeNodesForMode } from '../../lib/viewModes';
import { useProviderList } from '../../hooks/useProviderList';
import { hasSpawnableAgent } from '../../lib/groups';
import { CanvasEmptyState } from './CanvasEmptyState';

interface CanvasEmptyStateContainerProps {
  viewMode: ViewMode;
  lastNonSingleMode: NonSingleViewMode;
  selectedMeshId: number | null;
  activeNodeId: number | null;
  agentNodes: AgentNode[];
  visibleNodesLength: number;
}

export function CanvasEmptyStateContainer({
  viewMode,
  lastNonSingleMode,
  selectedMeshId,
  activeNodeId,
  agentNodes,
  visibleNodesLength,
}: CanvasEmptyStateContainerProps) {
  // The empty-state-only subscriptions — each fires a re-render of
  // THIS container, not the parent `AgentNodeView`. The parent only
  // sees the rendered output of `<CanvasEmptyState />`.
  const meshesCount = useMeshStore((s) => s.meshes.length);
  const providerList = useProviderList();
  const harnessReady = useMemo(() => hasSpawnableAgent(providerList), [providerList]);
  const openCreateMesh = useUIStore((s) => s.openCreateMesh);
  const openCanvasSpawnMenu = useUIStore((s) => s.openCanvasSpawnMenu);
  const resetGridControls = useUIStore((s) => s.resetGridControls);
  const openAppSettings = useUIStore((s) => s.openAppSettings);
  const setViewMode = useUIStore((s) => s.setViewMode);

  // `effectiveViewMode` collapses single → lastNonSingleMode only for
  // the SCOPE COUNT derivation (where the user-fell-back-from scope
  // matters). For all other purposes `viewMode` stays the original.
  // Closing over `lastNonSingleMode` from props (not subscribing to
  // it) is fine because `AgentNodeView` re-renders when it changes,
  // which re-renders this container.
  const effectiveViewMode: NonSingleViewMode = viewMode === 'single' ? lastNonSingleMode : viewMode;
  const scopedCount = useMemo(
    () => scopeNodesForMode(effectiveViewMode, agentNodes, selectedMeshId, activeNodeId).length,
    [effectiveViewMode, agentNodes, selectedMeshId, activeNodeId],
  );

  const input = useMemo(
    () => ({
      meshCount: meshesCount,
      totalNodeCount: agentNodes.length,
      scopedCount,
      filteredCount: visibleNodesLength,
      viewMode,
      selectedMeshId,
      harnessReady,
    }),
    [
      meshesCount,
      agentNodes.length,
      scopedCount,
      visibleNodesLength,
      viewMode,
      selectedMeshId,
      harnessReady,
    ],
  );

  // CLOSURE CONSISTENCY (senior-review round 4): `selectedMeshId` is
  // a prop, so closing over it from props is type-safe and avoids
  // the dep-array mismatch (was: listed in deps but ignored inside
  // the closure, which called `useMeshStore.getState().selectedMeshId`).
  // The `onOpenSpawnMenu` handler now closes over the prop value;
  // re-renders when `selectedMeshId` changes are fine because this
  // container only exists when the canvas is empty.
  const callbacks = useMemo(
    () => ({
      onCreateMesh: openCreateMesh,
      onOpenSpawnMenu: (meshId: number | null) => {
        // The caller-supplied id wins, else the sidebar selection,
        // else the first mesh in the list. Read lazily via
        // `getState()` inside the handler so the empty-state body
        // doesn't have to subscribe to `meshes[0].id` (those
        // subscriptions are only needed if the canvas is empty).
        const target =
          meshId
          ?? selectedMeshId
          ?? useMeshStore.getState().meshes[0]?.id
          ?? null;
        if (target !== null) openCanvasSpawnMenu(target);
      },
      onClearFilters: resetGridControls,
      onOpenSetup: openAppSettings,
      onViewAll: () => setViewMode('all'),
    }),
    [openCreateMesh, openCanvasSpawnMenu, resetGridControls, openAppSettings, setViewMode, selectedMeshId],
  );

  return <CanvasEmptyState input={input} callbacks={callbacks} />;
}
