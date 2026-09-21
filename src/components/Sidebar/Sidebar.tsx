import { useState, useRef, useMemo, useCallback } from 'react';
import { useMeshStore } from '../../stores/meshStore';
import { useAgentNodeStore, useAllAgentNodes, type AgentNode } from '../../stores/agentNodeStore';
import { useNodeActivityStore } from '../../stores/nodeActivityStore';
import { useUIStore } from '../../stores/uiStore';
import type { Mesh } from '../../stores/meshStore';
import { useProviderList } from '../../hooks/useProviderList';
import {
  DndContext,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
  type DragEndEvent,
} from '@dnd-kit/core';
import {
  SortableContext,
  sortableKeyboardCoordinates,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import { MeshItem } from './MeshItem';
import { dropdownId } from '../../lib/dropdownId';
import { useSidebarResize } from './useSidebarResize';
import { useClickOutside } from '../../hooks/useClickOutside';

// Issue #1748 — shared empty list so meshes without visible nodes keep a
// stable `meshNodes` reference instead of allocating a fresh `[]` per mesh
// on every render (which would defeat the memoized `MeshItem` rows below).
const EMPTY_NODES: AgentNode[] = [];

// Issue #1748 — module-level keyboard-sensor options so `useSensors` below
// returns a referentially stable array. An inline options literal would hand
// `DndContext` a new `sensors` prop on every render, re-rendering the whole
// provider subtree (and, through it, every sortable row) on each node update.
const KEYBOARD_SENSOR_OPTIONS = { coordinateGetter: sortableKeyboardCoordinates };

export function Sidebar() {
  const { width, isResizing, handleMouseDown } = useSidebarResize();
  // Single shared Spawn Option snapshot (issue #1502 — one fetch + one
  // `provider-list-changed` subscription for the whole app, no matter
  // how many headers mount). Platform filtering (e.g. macOS-only
  // Anthropic) is decided server-side via AgentProvider::available_on().
  // The backend already orders rows by `(is_terminal,
  // rank_of(harness_id))` with the full `mapBackendProviders` shape, so
  // `ProviderDropdown` grouping renders as-is (issues #575 / #583,
  // ADR-0016).
  const providerData = useProviderList();

  const meshes = useMeshStore(state => state.meshes);
  const selectedMeshId = useMeshStore(state => state.selectedMeshId);
  const selectMesh = useMeshStore(state => state.selectMesh);
  const reorderMeshes = useMeshStore(state => state.reorderMeshes);
  const getDefaultProvider = useMeshStore(state => state.getDefaultProvider);
  // Issue #1384 — `useAllAgentNodes` is the canonical derived selector.
  // Issue #1748 — group nodes per mesh in ONE memo pass instead of running
  // `filter` per mesh during render (O(M x N) work plus a fresh array per
  // row on every render, which would defeat the memoized `MeshItem` rows).
  // Per-node object identity comes from the store's shallow reconciliation
  // (`agentNodeStore.ts`), so untouched meshes keep identical element
  // references and their rows bail out via the `MeshItem` comparator.
  // Archived nodes are excluded here (issue #788 — they live in the
  // Archive probe tab, not the actionable sidebar list; mirrors mobile's
  // `visibleNodes` filter in src/mobile/screens/NodeList.tsx).
  const agentNodes = useAllAgentNodes();
  const nodesByMesh = useMemo(() => {
    const grouped = new Map<number, AgentNode[]>();
    for (const node of agentNodes) {
      if (node.status === 'archived') continue;
      const list = grouped.get(node.mesh_id);
      if (list) list.push(node);
      else grouped.set(node.mesh_id, [node]);
    }
    return grouped;
  }, [agentNodes]);
  const activeNodeId = useAgentNodeStore(state => state.activeNodeId);
  const activateNode = useNodeActivityStore(state => state.activateNode);
  const selectProviderForMesh = useAgentNodeStore(state => state.selectProviderForMesh);
  const deleteAgentNode = useAgentNodeStore(state => state.deleteAgentNode);
  // Issue #376 / #378: open the unified Probe Panel on a specific tab.
  // The `openProbeTab` helper atomically sets the tab and opens the panel.
  // The action reference is stable across renders (zustand), so we bind
  // the tab at the prop site without an extra closure layer.
  const openProbeTab = useUIStore(s => s.openProbeTab);

  // Issue #1264 — the open-id is a pre-prefixed string (built via
  // `dropdownId('mesh', mesh.id)`) so the sidebar spawn picker's
  // `data-dropdown-for` can't collide with a node- or terminal-keyed
  // menu that shares the mesh's numeric id. The toggle is mesh-id-keyed
  // for the open/close state but stores the prefixed value as the
  // single source of truth.
  const [openDropdownFor, setOpenDropdownFor] = useState<string | null>(null);
  // Both Sidebar `+ New mesh` buttons call the shared
  // `openCreateMesh` action — the modal itself is mounted at
  // App.tsx (gated on `uiStore.createMeshOpen`), not here.
  // The canvas empty state hits the same action.
  const openCreateMesh = useUIStore((s) => s.openCreateMesh);
  // Per-mesh "spawn in flight" set so the mesh row's `+ ▾` cluster shows
  // "Spawning…" and disables while `selectProviderForMesh` runs (an IPC
  // round-trip that includes worktree setup — seconds on a large repo).
  // The ref is the synchronous authority (guards a double-click that lands
  // in the same frame before the disabled state has re-rendered); the state
  // drives the render.
  const spawningMeshRef = useRef<Set<number>>(new Set());
  const [spawningMeshIds, setSpawningMeshIds] = useState<Set<number>>(new Set());

  // Close the provider dropdown when clicking outside of it.
  // Issue #492 — migrated to the shared `useClickOutside` hook. The
  // previous inline useEffect used a LOOSE `[data-dropdown-for]` selector
  // (no value), which would close the wrong dropdown if a future second
  // sidebar dropdown were added; the hook's scoped selector
  // `[data-dropdown-for="${openDropdownFor}"]` fixes that as a side effect.
  // Issue #1264 — the stored id is a pre-prefixed value (see the
  // `handleToggleDropdown` toggle below + the `NodeCreationForm`
  // consumer) so the sidebar's spawn-picker menu can't collide with
  // any other surface that shares the mesh's numeric id.
  useClickOutside<string>(openDropdownFor, () => setOpenDropdownFor(null));

  // Issue #1748 — row callbacks are `useCallback`-stable so the memoized
  // `MeshItem` rows actually skip: `selectMesh`/`openProbeTab`/`activateNode`
  // are stable zustand actions, and the two pieces of render state read here
  // (`selectedMeshId`, `openDropdownFor`) go through `getState`/functional
  // updates instead of the dependency array.
  const handleSelectMesh = useCallback((meshId: number) => {
    const current = useMeshStore.getState().selectedMeshId;
    selectMesh(current === meshId ? null : meshId);
  }, [selectMesh]);
  const handleToggleDropdown = useCallback((mesh: Mesh) => {
    const key = dropdownId('mesh', mesh.id);
    setOpenDropdownFor(prev => (prev === key ? null : key));
  }, []);

  // Issue #375 — the right-click "Properties" entry opens the Probe
  // Panel on the ⚙️ Mesh Properties tab. We select the mesh first so
  // `useProbeContext` resolves to the right row, then flip the probe open.
  // Issue #767 split out the drift `!` badge (see handleOpenWorktreesProbe
  // below) — the two intents ("edit config" vs "fix the drift") must
  // not share a handler.
  const handleOpenFilesProbe = useCallback(() => {
    openProbeTab('files');
  }, [openProbeTab]);

  const handleOpenPropertiesProbe = useCallback((meshId: number) => {
    selectMesh(meshId);
    openProbeTab('properties');
  }, [selectMesh, openProbeTab]);

  // Issue #767 — the drift `!` badge in the sidebar opens the Probe
  // Panel on the 🌳 Worktree Manager tab, where the HealthBlock's
  // Restore/Free actions live. The badge's intent is "your mesh is
  // drifted and needs recovery"; the Properties tab has no such
  // controls, so routing there (the pre-#767 behaviour) was a dead-end.
  const handleOpenWorktreesProbe = useCallback((meshId: number) => {
    selectMesh(meshId);
    openProbeTab('worktrees');
  }, [selectMesh, openProbeTab]);

  // Issue #378 — the right-click "GitHub Issues" and "Archive" entries
  // open the Probe Panel on the 🐙 / 🕒 tabs respectively. The mesh is
  // selected first (same dance as the Properties entry point) so
  // `useProbeContext` resolves to the right row before the tab mounts.
  const handleOpenIssuesProbe = useCallback((meshId: number) => {
    selectMesh(meshId);
    openProbeTab('issues');
  }, [selectMesh, openProbeTab]);
  const handleOpenSessionHistoryProbe = useCallback((meshId: number) => {
    selectMesh(meshId);
    openProbeTab('sessions');
  }, [selectMesh, openProbeTab]);

  const handleSelectProvider = useCallback(async (mesh: Mesh, providerId: string, useWorktree?: boolean, configurationId?: string) => {
    setOpenDropdownFor(null);
    // Guard against a double-spawn: if a spawn for this mesh is already in
    // flight, ignore the click (the button is also disabled once the state
    // re-renders, but the ref covers the same-frame race).
    if (spawningMeshRef.current.has(mesh.id)) return;
    spawningMeshRef.current.add(mesh.id);
    setSpawningMeshIds(new Set(spawningMeshRef.current));
    // The create→activate→select-mesh dance + its rollback contract live in
    // selectProviderForMesh (issue #283); this handler stays a thin UI shim.
    try {
      const node = await selectProviderForMesh(mesh.id, mesh.name, mesh.path, providerId, useWorktree, undefined, configurationId);
      activateNode(node.id);
    } catch (e) {
      console.error('Failed to create node:', e);
    } finally {
      spawningMeshRef.current.delete(mesh.id);
      setSpawningMeshIds(new Set(spawningMeshRef.current));
    }
  }, [selectProviderForMesh, activateNode]);

  const handleDeleteNode = useCallback(async (e: React.MouseEvent, nodeId: number) => {
    e.stopPropagation();
    await deleteAgentNode(nodeId);
  }, [deleteAgentNode]);

  const handleDragEnd = useCallback((event: DragEndEvent) => {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const activeIndex = meshes.findIndex(p => p.id === active.id);
    const overIndex = meshes.findIndex(p => p.id === over.id);
    if (activeIndex === -1 || overIndex === -1) return;
    reorderMeshes(active.id as number, overIndex);
  }, [meshes, reorderMeshes]);

  // Issue #727 — register KeyboardSensor alongside the default
  // PointerSensor so the mesh-reorder drag handle is operable from the
  // keyboard. `sortableKeyboardCoordinates` (from `@dnd-kit/sortable`)
  // walks the active row across siblings on ArrowUp/Down — the generic
  // defaultCoordinateGetter would translate freely, which doesn't fit a
  // vertical list. Space picks up the focused handle, Enter picks it
  // up too, Arrow keys move, Escape drops the item back where it
  // started. No options on PointerSensor — matches the dnd-kit default
  // sensor set so existing pointer behaviour (drag starts on
  // pointerdown of the handle, clicks elsewhere on the row still
  // select the mesh) is unchanged.
  const sensors = useSensors(
    useSensor(PointerSensor),
    useSensor(KeyboardSensor, KEYBOARD_SENSOR_OPTIONS),
  );

  // Issue #1748 — `SortableContext` keys its context value on the `items`
  // array identity, and every `MeshItem` subscribes through `useSortable` —
  // a fresh `meshes.map(...)` per render would re-render every row via
  // context even with memoized props. The id list only changes when the
  // mesh list itself changes, never on node updates.
  const sortableMeshIds = useMemo(() => meshes.map(p => p.id), [meshes]);

  return (
    <div className="relative flex h-full" style={{ width }}>
      <div
        onMouseDown={handleMouseDown}
        role="separator"
        aria-orientation="vertical"
        aria-label="Resize sidebar"
        className={`absolute top-0 -right-1 w-2.5 h-full cursor-col-resize z-10 after:absolute after:inset-y-0 after:left-1 after:w-0.5 after:transition-colors ${
          isResizing ? 'after:bg-accent-cyan/60' : 'after:bg-transparent hover:after:bg-accent-cyan/40'
        }`}
      />

      <div className="w-full bg-bg-surface border-r border-border-subtle flex flex-col h-full overflow-hidden">
        {/* Meshes list — the Mesh Create modal itself is mounted at App.tsx
            scope (driven by `uiStore.createMeshOpen`); both Sidebar
            buttons and the canvas empty state's "New mesh" CTA hit the
            same `openCreateMesh` action. */}
        <div className="flex-1 overflow-y-auto">
          <div className="p-2">
            {meshes.length === 0 ? (
              <div className="flex flex-col items-center gap-3 px-2 py-8 text-center">
                <p className="text-xs text-text-muted font-sans">
                  No meshes yet. Add a repository to start orchestrating agents.
                </p>
                <button
                  type="button"
                  onClick={openCreateMesh}
                  className="px-3 py-1.5 text-xs font-medium text-accent-cyan bg-accent-cyan/10 hover:bg-accent-cyan/20 border border-accent-cyan/20 rounded-md transition-colors"
                >
                  + New mesh
                </button>
              </div>
            ) : (
              <DndContext sensors={sensors} onDragEnd={handleDragEnd}>
                <SortableContext items={sortableMeshIds} strategy={verticalListSortingStrategy}>
                  {meshes.map(mesh => (
                    <MeshItem
                      key={mesh.id}
                      mesh={mesh}
                      isSelected={selectedMeshId === mesh.id}
                      isDropdownOpen={openDropdownFor === dropdownId('mesh', mesh.id)}
                      isSpawning={spawningMeshIds.has(mesh.id)}
                      providerList={providerData}
                      onSelectMesh={handleSelectMesh}
                      onNewNode={handleToggleDropdown}
                      onSelectProvider={handleSelectProvider}
                      onOpenFilesProbe={handleOpenFilesProbe}
                      onOpenPropertiesProbe={handleOpenPropertiesProbe}
                      onOpenWorktreesProbe={handleOpenWorktreesProbe}
                      onOpenIssuesProbe={handleOpenIssuesProbe}
                      onOpenSessionHistoryProbe={handleOpenSessionHistoryProbe}
                      meshNodes={nodesByMesh.get(mesh.id) ?? EMPTY_NODES}
                      activeNodeId={activeNodeId}
                      onActivateNode={activateNode}
                      selectMesh={selectMesh}
                      onDeleteNode={handleDeleteNode}
                      getDefaultProvider={getDefaultProvider}
                    />
                  ))}
                </SortableContext>
              </DndContext>
            )}
          </div>
        </div>

        {/* Add mesh */}
        <button
          onClick={openCreateMesh}
          className="w-full px-3 py-2.5 flex items-center justify-center gap-1.5 text-xs font-sans text-accent-cyan hover:text-accent-blue border-t border-dashed border-border-subtle hover:bg-bg-card/40 transition-colors"
        >
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
            <line x1="12" y1="5" x2="12" y2="19"/>
            <line x1="5" y1="12" x2="19" y2="12"/>
          </svg>
          New mesh
        </button>
      </div>
    </div>
  );
}
