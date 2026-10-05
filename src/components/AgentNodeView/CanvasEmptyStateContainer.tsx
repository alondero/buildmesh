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
 * already-subscribed-to values — `viewMode`, `selectedMeshId`,
 * `agentNodes`, plus the `scope` the parent derived for this render)
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
import type { ViewMode } from '../../stores/uiStore';
import { useMeshStore } from '../../stores/meshStore';
import { useNodeActivityStore } from '../../stores/nodeActivityStore';
import { ReadinessSteps } from './ReadinessSteps';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useUIStore } from '../../stores/uiStore';
import type { DerivedScope } from '../../lib/viewModes';
import { useProviderList } from '../../hooks/useProviderList';
import { hasSpawnableAgent } from '../../lib/groups';
import { CanvasEmptyState } from './CanvasEmptyState';

interface CanvasEmptyStateContainerProps {
  viewMode: ViewMode;
  selectedMeshId: number | null;
  agentNodes: AgentNode[];
  /** The scope `AgentNodeView` already derived for this render (#2071).
   *  The counts below come straight from it, so the empty state can never
   *  count a different scope than the grid rendered. */
  scope: DerivedScope;
}

export function CanvasEmptyStateContainer({
  viewMode,
  selectedMeshId,
  agentNodes,
  scope,
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

  // #2071 — the two counts the classifier needs, straight off the derived
  // scope: how many nodes the scope holds before the controls narrow it,
  // and how many survive. `scopeNodesForMode` is no longer called here —
  // this surface never re-derives scope of its own (#2071).
  const input = useMemo(
    () => ({
      meshCount: meshesCount,
      totalNodeCount: agentNodes.length,
      scopedCount: scope.scopedNodeCount,
      filteredCount: scope.visibleNodeCount,
      viewMode,
      selectedMeshId,
      harnessReady,
    }),
    [
      meshesCount,
      agentNodes.length,
      scope.scopedNodeCount,
      scope.visibleNodeCount,
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
      onOpenSetup: () => openAppSettings('providers'),
      onViewAll: () => setViewMode('all'),
      onOpenHelp: () => useUIStore.getState().toggleCheatsheet(),
      onOpenTerminal: () => {
        const mesh = useMeshStore.getState().meshes.find(m => m.id === selectedMeshId) ?? useMeshStore.getState().meshes[0];
        if (!mesh) return;
        return useAgentNodeStore.getState().selectProviderForMesh(mesh.id, mesh.name, mesh.path, 'terminal')
          .then(node => useNodeActivityStore.getState().activateNode(node.id))
          // The store reports creation failures through App's error toast.
          .catch(() => {});
      },
    }),
    [openCreateMesh, openCanvasSpawnMenu, resetGridControls, openAppSettings, setViewMode, selectedMeshId],
  );

  if (meshesCount > 0 && scope.scopedNodeCount === 0 && viewMode !== 'pinned') {
    return <div className="flex-1 min-h-0 overflow-y-auto flex flex-col p-4">
      <div className="m-auto w-full max-w-sm"><CanvasEmptyState input={input} callbacks={callbacks} />
        <ReadinessSteps repositoryReady harnessReady={harnessReady} callbacks={callbacks} />
      </div>
    </div>;
  }
  return <CanvasEmptyState input={input} callbacks={callbacks} />;
}
