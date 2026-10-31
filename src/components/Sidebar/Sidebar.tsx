import { useState, useRef, useMemo, useCallback } from 'react';
import { useMeshStore } from '../../stores/meshStore';
import { useAgentNodeStore, useAllAgentNodes, type AgentNode } from '../../stores/agentNodeStore';
import { useNodeActivityStore } from '../../stores/nodeActivityStore';
import { enterMesh, focusNode } from '../../lib/navigation';
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
import { NodeActivityCluster, clusterActivityNodes } from '../../lib/nodeActivities';
import { MeshItem } from './MeshItem';
import { dropdownId } from '../../lib/dropdownId';
import { useSidebarResize } from './useSidebarResize';
import { useClickOutside } from '../../hooks/useClickOutside';

// Shared empty cluster list so meshes without visible nodes keep a stable
// `nodeClusters` reference instead of allocating a fresh `[]` per mesh on every
// render (which would defeat the memoized `MeshItem` rows below — issue #1748).
const EMPTY_CLUSTERS: NodeActivityCluster[] = [];

// Issue #1748 — module-level keyboard-sensor options so `useSensors` below
// returns a referentially stable array. An inline options literal would hand
// `DndContext` a new `sensors` prop on every render, re-rendering the whole
// provider subtree (and, through it, every sortable row) on each node update.
const KEYBOARD_SENSOR_OPTIONS = { coordinateGetter: sortableKeyboardCoordinates };

// Issue #1939 — stable empty identity for the sortable-id retention below,
// so the band-empty initial state shares one reference like `EMPTY_CLUSTERS`.
const EMPTY_SORTABLE_IDS: number[] = [];

// The mesh row's colour bar doubles as the reorder handle (click = colour
// picker, drag = reorder), so a press must travel 5px before a drag activates
// and the click passes through to the picker. Module-level for the same
// referential stability `KEYBOARD_SENSOR_OPTIONS` above needs (issue #1748).
const POINTER_SENSOR_OPTIONS = { activationConstraint: { distance: 5 } };

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
  // Issue #1939 — the same pass also partitions the meshes into the
  // active band (at least one open node, i.e. a non-empty group) and the
  // inactive band. Both bands keep the manual drag order (`meshes` order):
  // a mesh only ever crosses a band boundary, never reorders among its
  // peers. Membership is an emptiness test on the group — no status
  // interpretation participates — so it holds for both the close path
  // (row deleted) and the archive path (status set, excluded above).
  const agentNodes = useAllAgentNodes();
  const circuitOwnerships = useAgentNodeStore(state => state.circuitOwnerships);
  const manualGroups = useNodeActivityStore(state => state.groups);
  // Paired agents share one Node Activity card in the grid. The same pass that
  // partitions nodes per mesh now also resolves each mesh's nodes into those
  // cards, so the sidebar can cluster them under a connector rail instead of
  // listing unpaired-looking rows. `clusterActivityNodes` reads the identical
  // `activityRootId` resolver the grid uses, so the two surfaces cannot drift
  // apart about who is paired with whom.
  //
  // Kept inside the existing single memo (issue #1748) rather than a per-mesh
  // filter during render: that would cost O(M x N) work and hand every
  // `MeshItem` a fresh array per render, defeating the memoized rows.
  //
  // The band split itself is unchanged and still tests emptiness on the flat
  // per-mesh list — a paired cluster is as "active" as its members were.
  const { clustersByMesh, activeMeshes, inactiveMeshes } = useMemo(() => {
    const grouped = new Map<number, AgentNode[]>();
    for (const node of agentNodes) {
      if (node.status === 'archived') continue;
      const list = grouped.get(node.mesh_id);
      if (list) list.push(node);
      else grouped.set(node.mesh_id, [node]);
    }
    const clusters = new Map<number, NodeActivityCluster[]>();
    for (const [meshId, nodes] of grouped) {
      clusters.set(meshId, clusterActivityNodes(nodes, circuitOwnerships, manualGroups));
    }
    const active: Mesh[] = [];
    const inactive: Mesh[] = [];
    for (const mesh of meshes) {
      const group = grouped.get(mesh.id);
      if (group && group.length > 0) active.push(mesh);
      else inactive.push(mesh);
    }
    return { clustersByMesh: clusters, activeMeshes: active, inactiveMeshes: inactive };
  }, [meshes, agentNodes, circuitOwnerships, manualGroups]);
  // Issue #1748 — `Sidebar` no longer subscribes to `activeNodeId`: each
  // `NodeItem` owns its own active bit, so activating a node re-renders
  // only the rows whose bit flips instead of the whole sidebar.
  const activateNode = useCallback((nodeId: number) => { focusNode(nodeId, { view: 'mesh' }); }, []);
  const selectProviderForMesh = useAgentNodeStore(state => state.selectProviderForMesh);
  const deleteAgentNode = useAgentNodeStore(state => state.deleteAgentNode);
  // Issue #1264 — the open-id is a pre-prefixed string (built via
  // `dropdownId('mesh', mesh.id)`) so the sidebar spawn picker's
  // `data-dropdown-for` can't collide with a node- or terminal-keyed
  // menu that shares the mesh's numeric id. The toggle is mesh-id-keyed
  // for the open/close state but stores the prefixed value as the
  // single source of truth.
  const [openDropdownFor, setOpenDropdownFor] = useState<string | null>(null);
  // The Sidebar's mesh-creation CTA calls the shared `openCreateMesh`
  // action — the modal itself is mounted at App.tsx (gated on
  // `uiStore.createMeshOpen`), not here. The canvas empty state hits
  // the same action.
  const openCreateMesh = useUIStore((s) => s.openCreateMesh);
  // Issue #2468 — the empty workspace renders its own "+ New mesh" CTA
  // inline, directly under the "No meshes yet" message. The sticky
  // footer rendered a second mesh-creation button in the same 256px
  // column, so an empty workspace showed two CTAs in one place (issue
  // #2468). The footer stands down while the inline CTA is showing, and
  // returns once the workspace holds a mesh.
  const inlineEmptyCtaVisible = meshes.length === 0;
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
  // `MeshItem` rows actually skip: navigation intents and `openProbeTab`
  // have stable references, and the two pieces of render state read here
  // (`selectedMeshId`, `openDropdownFor`) go through `getState`/functional
  // updates instead of the dependency array.
  //
  const handleSelectMesh = enterMesh;
  const handleToggleDropdown = useCallback((mesh: Mesh) => {
    const key = dropdownId('mesh', mesh.id);
    setOpenDropdownFor(prev => (prev === key ? null : key));
  }, []);

  // Each right-click Probe destination — Properties, File Explorer, Archive,
  // GitHub Issues — names the Mesh it acts on: `enterMesh` selects that Mesh
  // (so `useProbeContext` resolves to the right row) and flips the panel to
  // the destination tab in one step. Issue #375 owns the Properties entry;
  // #767 split out the drift `!` badge (see handleOpenWorktreesProbe below)
  // because the two intents ("edit config" vs "fix the drift") must not
  // share a handler.
  // Issue #2459 — "File Explorer" joined that set. It used to call bare
  // `openProbeTab('files')`, so right-clicking an *unselected* Mesh
  // opened Project Files for whichever Mesh was already active (or
  // reported no subject when none was).
  const handleOpenFilesProbe = useCallback((meshId: number) => {
    enterMesh(meshId, { probeTab: 'files' });
  }, []);

  const handleOpenPropertiesProbe = useCallback((meshId: number) => {
    enterMesh(meshId, { probeTab: 'properties' });
  }, []);

  // Issue #767 — the drift `!` badge in the sidebar opens the Probe
  // Panel on the 🌳 Worktree Manager tab, where the HealthBlock's
  // Restore/Free actions live. The badge's intent is "your mesh is
  // drifted and needs recovery"; the Properties tab has no such
  // controls, so routing there (the pre-#767 behaviour) was a dead-end.
  const handleOpenWorktreesProbe = useCallback((meshId: number) => {
    enterMesh(meshId, { probeTab: 'worktrees' });
  }, []);

  // Issue #378 — the right-click "GitHub Issues" and "Archive" entries
  // open the Probe Panel on the 🐙 / 🕒 tabs respectively. The mesh is
  // scoped first (same dance as the Properties entry point) so
  // `useProbeContext` resolves to the right row before the tab mounts.
  const handleOpenIssuesProbe = useCallback((meshId: number) => {
    enterMesh(meshId, { probeTab: 'issues' });
  }, []);
  const handleOpenSessionHistoryProbe = useCallback((meshId: number) => {
    enterMesh(meshId, { probeTab: 'sessions' });
  }, []);

  const handleSelectProvider = useCallback(async (mesh: Mesh, providerId: string, useWorktree?: boolean, configurationId?: string, loginId?: string) => {
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
      await selectProviderForMesh(mesh.id, mesh.name, mesh.path, providerId, useWorktree, undefined, configurationId, loginId);
    } catch (e) {
      console.error('Failed to create node:', e);
    } finally {
      spawningMeshRef.current.delete(mesh.id);
      setSpawningMeshIds(new Set(spawningMeshRef.current));
    }
  }, [selectProviderForMesh]);

  const handleDeleteNode = useCallback(async (e: React.MouseEvent, nodeId: number) => {
    e.stopPropagation();
    await deleteAgentNode(nodeId);
  }, [deleteAgentNode]);

  // Issue #1939 — the drop target resolves against the MANUAL `meshes`
  // order, not the displayed band order, so dragging across the band
  // boundary only changes the manual position; band membership (open-node
  // count) is never a drag side effect and needs no special handling.
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
  // vertical list. Space picks up the focused bar, Arrow keys move, Escape
  // drops the item back where it started (Enter opens the colour picker
  // instead, per `MeshItem`'s keydown split). PointerSensor carries a 5px
  // activation distance so a click on the same bar reaches the picker; a
  // drag starts after 5px of travel, and clicks elsewhere on the row still
  // select the mesh.
  const sensors = useSensors(
    useSensor(PointerSensor, POINTER_SENSOR_OPTIONS),
    useSensor(KeyboardSensor, KEYBOARD_SENSOR_OPTIONS),
  );

  // Issue #1748 — `SortableContext` keys its context value on the `items`
  // array identity, and every `MeshItem` subscribes through `useSortable` —
  // a fresh id list per render would re-render every row via context even
  // with memoized props. The id list only changes when the displayed order
  // actually changes, never on band-neutral node updates.
  // Issue #1939 — the items follow the DISPLAYED order (active band first)
  // so the keyboard sensor walks rows as the user sees them. The band
  // arrays above are fresh literals on every node update, so mapping them
  // directly would mint a new identity on each one. The previous list is
  // retained while the joined id signature is unchanged (element-wise
  // comparison, idempotent across re-renders), so identity flips only when
  // a mesh actually crosses a band boundary or the mesh list itself
  // changes. `handleDragEnd` below still resolves the drop target against
  // the manual `meshes` order, so dragging across the band boundary only
  // changes the manual position — band membership itself is never
  // draggable.
  const sortableMeshIdsPrevRef = useRef<number[]>(EMPTY_SORTABLE_IDS);
  const nextSortableMeshIds = [...activeMeshes, ...inactiveMeshes].map(p => p.id);
  const prevSortableMeshIds = sortableMeshIdsPrevRef.current;
  if (
    prevSortableMeshIds.length !== nextSortableMeshIds.length ||
    prevSortableMeshIds.some((id, index) => id !== nextSortableMeshIds[index])
  ) {
    sortableMeshIdsPrevRef.current = nextSortableMeshIds;
  }
  const sortableMeshIds = sortableMeshIdsPrevRef.current;

  // Issue #1939 — one row renderer for both bands. Inactive rows render
  // `dimmed` (presentational only: same structure, same spawn affordance).
  const renderMeshRow = (mesh: Mesh, dimmed: boolean) => (
    <MeshItem
      key={mesh.id}
      mesh={mesh}
      isSelected={selectedMeshId === mesh.id}
      isDropdownOpen={openDropdownFor === dropdownId('mesh', mesh.id)}
      isSpawning={spawningMeshIds.has(mesh.id)}
      dimmed={dimmed}
      providerList={providerData}
      onSelectMesh={handleSelectMesh}
      onNewNode={handleToggleDropdown}
      onSelectProvider={handleSelectProvider}
      onOpenFilesProbe={handleOpenFilesProbe}
      onOpenPropertiesProbe={handleOpenPropertiesProbe}
      onOpenWorktreesProbe={handleOpenWorktreesProbe}
      onOpenIssuesProbe={handleOpenIssuesProbe}
      onOpenSessionHistoryProbe={handleOpenSessionHistoryProbe}
      nodeClusters={clustersByMesh.get(mesh.id) ?? EMPTY_CLUSTERS}
      onActivateNode={activateNode}
      onDeleteNode={handleDeleteNode}
      getDefaultProvider={getDefaultProvider}
    />
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
        {/* Meshes list — the Mesh Create modal itself is mounted at App.tsx
            scope (driven by `uiStore.createMeshOpen`); the Sidebar's mesh
            CTA (inline here when the workspace is empty, otherwise the
            sticky footer) and the canvas empty state's "New mesh" CTA hit
            the same `openCreateMesh` action. */}
        <div className="flex-1 min-h-0 overflow-y-auto overflow-x-hidden">
          <div className="p-2">
            {meshes.length === 0 ? (
              <div className="flex flex-col items-center gap-3 px-2 py-8 text-center">
                <p className="text-xs text-text-muted font-sans">
                  No meshes yet. Add a repository to start orchestrating agents.
                </p>
                {/* Issue #2468 — this is the Sidebar's only mesh-creation CTA
                    while the workspace is empty; the sticky footer stands
                    down so the column never shows two of them. */}
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
                  {/* Issue #1939 — meshes with at least one open node form
                      the top band; every other mesh follows below, dimmed,
                      in manual drag order. The band carries no label: a
                      mesh's position in the list is the affordance, so the
                      list spends no vertical space on a heading that names
                      what the ordering already shows. One scroll container
                      throughout. */}
                  {activeMeshes.length > 0
                    ? (
                      <>
                        {activeMeshes.map(mesh => renderMeshRow(mesh, false))}
                        {inactiveMeshes.map(mesh => renderMeshRow(mesh, true))}
                      </>
                    )
                    : meshes.map(mesh => renderMeshRow(mesh, false))}
                </SortableContext>
              </DndContext>
            )}
          </div>
        </div>

        {/* Add mesh — issue #2468. The empty workspace already shows a
            "+ New mesh" button inline, right under the "No meshes yet"
            message, so the sticky footer stands down there instead of
            stacking a second mesh-creation CTA in the same column. The
            footer returns as soon as the workspace holds a mesh. */}
        {!inlineEmptyCtaVisible && (
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
        )}
      </div>
    </div>
  );
}
