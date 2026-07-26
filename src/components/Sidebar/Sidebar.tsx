import { useState, useEffect, useCallback, useRef } from 'react';
import { useMeshStore } from '../../stores/meshStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useUIStore } from '../../stores/uiStore';
import type { Mesh } from '../../stores/meshStore';
import { listProviders } from '../../lib/tauri';
import { useProviderListInvalidation } from '../../hooks/useProviderListInvalidation';
import { MeshCreateModal } from '../Mesh/MeshCreateModal';
import { defaultMeshColor } from '../../lib/meshColors';
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
import { mapBackendProviders, type SpawnOption } from '../../lib/groups';
import { useSidebarResize } from './useSidebarResize';
import { useClickOutside } from '../../hooks/useClickOutside';

export function Sidebar() {
  const { width, isResizing, handleMouseDown } = useSidebarResize();
  const [providerData, setProviderData] = useState<SpawnOption[]>([]);

  // Fetch available providers from the backend. Platform filtering (e.g. macOS-only
  // Anthropic) is decided server-side via AgentProvider::available_on().
  // Refetchable so the spawn menu picks up providers added in App Settings
  // without an app restart — the settings modal emits `provider-list-changed`
  // on upsert/remove (the tauri.ts cache bust alone doesn't reach us, since
  // we hold our own local snapshot), and this hook re-fetches on the signal.
  const refreshProviders = useCallback(() => {
    listProviders()
      // Issue #575 / ADR-0016 — preserve the full Spawn Option shape so
      // `ProviderDropdown` can group by `group_key` and render the
      // harness header vs Proxied child rows. The backend already orders
      // rows by `(is_terminal, rank_of(harness_id))`, so the flat list
      // carries both the order and the grouping data — the frontend is
      // a pure render. The 8-field projection lives in
      // `mapBackendProviders` (issue #583 cleanup).
      .then(backendProviders => setProviderData(mapBackendProviders(backendProviders)))
      .catch(err => console.error('listProviders failed:', err));
  }, []);

  useEffect(() => { refreshProviders(); }, [refreshProviders]);
  useProviderListInvalidation(refreshProviders);

  const meshes = useMeshStore(state => state.meshes);
  const selectedMeshId = useMeshStore(state => state.selectedMeshId);
  const selectMesh = useMeshStore(state => state.selectMesh);
  const reorderMeshes = useMeshStore(state => state.reorderMeshes);
  const getDefaultProvider = useMeshStore(state => state.getDefaultProvider);
  const agentNodes = useAgentNodeStore(state => state.agentNodes);
  const activeNodeId = useAgentNodeStore(state => state.activeNodeId);
  const setActiveNode = useAgentNodeStore(state => state.setActiveNode);
  const selectProviderForMesh = useAgentNodeStore(state => state.selectProviderForMesh);
  const deleteAgentNode = useAgentNodeStore(state => state.deleteAgentNode);
  // Issue #376 / #378: open the unified Probe Panel on a specific tab.
  // The `openProbeTab` helper atomically sets the tab and opens the panel.
  // The action reference is stable across renders (zustand), so we bind
  // the tab at the prop site without an extra closure layer.
  const openProbeTab = useUIStore(s => s.openProbeTab);

  const [openDropdownFor, setOpenDropdownFor] = useState<number | null>(null);
  const [createMeshOpen, setCreateMeshOpen] = useState(false);
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
  useClickOutside(openDropdownFor, () => setOpenDropdownFor(null));

  const handleSelectMesh = (meshId: number) => selectMesh(selectedMeshId === meshId ? null : meshId);
  const handleToggleDropdown = (mesh: Mesh) => setOpenDropdownFor(openDropdownFor === mesh.id ? null : mesh.id);

  // Issue #375 — the right-click "Properties" entry opens the Probe
  // Panel on the ⚙️ Mesh Properties tab. We select the mesh first so
  // `useProbeContext` resolves to the right row, then flip the probe open.
  // Issue #767 split out the drift `!` badge (see handleOpenWorktreesProbe
  // below) — the two intents ("edit config" vs "fix the drift") must
  // not share a handler.
  const handleOpenPropertiesProbe = (meshId: number) => {
    selectMesh(meshId);
    openProbeTab('properties');
  };

  // Issue #767 — the drift `!` badge in the sidebar opens the Probe
  // Panel on the 🌳 Worktree Manager tab, where the HealthBlock's
  // Restore/Free actions live. The badge's intent is "your mesh is
  // drifted and needs recovery"; the Properties tab has no such
  // controls, so routing there (the pre-#767 behaviour) was a dead-end.
  const handleOpenWorktreesProbe = (meshId: number) => {
    selectMesh(meshId);
    openProbeTab('worktrees');
  };

  // Issue #378 — the right-click "GitHub Issues" and "Archive" entries
  // open the Probe Panel on the 🐙 / 🕒 tabs respectively. The mesh is
  // selected first (same dance as the Properties entry point) so
  // `useProbeContext` resolves to the right row before the tab mounts.
  const handleOpenIssuesProbe = (meshId: number) => {
    selectMesh(meshId);
    openProbeTab('issues');
  };
  const handleOpenSessionHistoryProbe = (meshId: number) => {
    selectMesh(meshId);
    openProbeTab('sessions');
  };

  const handleSelectProvider = async (mesh: Mesh, providerId: string, useWorktree?: boolean) => {
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
      await selectProviderForMesh(mesh.id, mesh.name, mesh.path, providerId, useWorktree);
    } catch (e) {
      console.error('Failed to create node:', e);
    } finally {
      spawningMeshRef.current.delete(mesh.id);
      setSpawningMeshIds(new Set(spawningMeshRef.current));
    }
  };

  const handleDeleteNode = async (e: React.MouseEvent, nodeId: number) => {
    e.stopPropagation();
    await deleteAgentNode(nodeId);
  };

  const handleDragEnd = (event: DragEndEvent) => {
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const activeIndex = meshes.findIndex(p => p.id === active.id);
    const overIndex = meshes.findIndex(p => p.id === over.id);
    if (activeIndex === -1 || overIndex === -1) return;
    reorderMeshes(active.id as number, overIndex);
  };

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
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );

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
        {createMeshOpen && (
          <MeshCreateModal
            onClose={() => setCreateMeshOpen(false)}
            defaultColor={defaultMeshColor(meshes.length)}
          />
        )}

        {/* Meshes list */}
        <div className="flex-1 overflow-y-auto">
          <div className="p-2">
            {meshes.length === 0 ? (
              <div className="flex flex-col items-center gap-3 px-2 py-8 text-center">
                <p className="text-xs text-text-muted font-sans">
                  No meshes yet. Add a repository to start orchestrating agents.
                </p>
                <button
                  type="button"
                  onClick={() => setCreateMeshOpen(true)}
                  className="px-3 py-1.5 text-xs font-medium text-accent-cyan bg-accent-cyan/10 hover:bg-accent-cyan/20 border border-accent-cyan/20 rounded-md transition-colors"
                >
                  + New mesh
                </button>
              </div>
            ) : (
              <DndContext sensors={sensors} onDragEnd={handleDragEnd}>
                <SortableContext items={meshes.map(p => p.id)} strategy={verticalListSortingStrategy}>
                  {meshes.map(mesh => (
                    <MeshItem
                      key={mesh.id}
                      mesh={mesh}
                      isSelected={selectedMeshId === mesh.id}
                      isDropdownOpen={openDropdownFor === mesh.id}
                      isSpawning={spawningMeshIds.has(mesh.id)}
                      providerList={providerData}
                      onSelectMesh={handleSelectMesh}
                      onNewNode={handleToggleDropdown}
                      onSelectProvider={handleSelectProvider}
                      onOpenFilesProbe={() => openProbeTab('files')}
                      onOpenPropertiesProbe={handleOpenPropertiesProbe}
                      onOpenWorktreesProbe={handleOpenWorktreesProbe}
                      onOpenIssuesProbe={handleOpenIssuesProbe}
                      onOpenSessionHistoryProbe={handleOpenSessionHistoryProbe}
                      // Issue #788 — archived nodes live in the Archive probe
                      // tab, not the actionable sidebar list (mirrors mobile's
                      // `visibleNodes` filter in src/mobile/screens/NodeList.tsx).
                      meshNodes={agentNodes.filter(w => w.mesh_id === mesh.id && w.status !== 'archived')}
                      activeNodeId={activeNodeId}
                      setActiveNode={setActiveNode}
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
          onClick={() => setCreateMeshOpen(true)}
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
