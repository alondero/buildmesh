import { formatError } from '../lib/errorUtils';
import { create } from 'zustand';
import * as api from '../lib/tauri';
import type { Mesh } from '../types/generated/Mesh';
// Issue #1247 — `deleteMesh` now also (a) refetches the agent-node list so
// ghost rows drop out of the grid + sidebar, (b) disposes the xterm for
// every doomed node (the dispose-on-delete invariant is satisfied because
// the row IS deleted — same pattern as `agentNodeStore.deleteAgentNode`'s
// success-path dispose, issue #647), and (c) nulls `selectedMeshId` /
// `activeNodeId` when they pointed into the deleted mesh. The cross-store
// reach pattern matches the four sites already documented at the top of
// `agentNodeStore.ts` (#1054); adding this as a fifth reach-through keeps
// the store layer the single source of truth without inventing a new
// message-bus layer just for one method.
//
// BOTH `useAgentNodeStore` AND `disposeTerminal` are intentionally
// resolved lazily via `await import(...)` inside `deleteMesh` rather
// than imported at module-load. Adding either as a static import
// introduces an ESM cycle that crashes uiStore's `create()` callback:
//
//   meshStore → agentNodeStore → Terminal.tsx → uiStore → meshStore
//
// In that cycle, uiStore's `create()` callback at uiStore.ts:353 runs
// `useMeshStore.getState()` while `useMeshStore` is still `undefined`
// (meshStore is mid-load) and throws. Pre-#1247 meshStore imported
// neither module at module-load, so the cycle didn't form when
// something imported `useMeshStore` first (e.g. uiStore.ts itself —
// the natural entry chain was meshStore → uiStore, with meshStore
// finishing before uiStore read its exports).
//
// `agentNodeStore.ts:4` can still eagerly import `disposeTerminal`
// because the natural entry chain runs Terminal FIRST, then uiStore,
// then meshStore, then agentNodeStore — by the time uiStore's
// `create()` callback reads `useMeshStore`, meshStore has already
// called `create()`. meshStore is a sink in the original load order;
// making it import agentNodeStore (which imports Terminal) inverts the
// chain and creates the cycle.
//
// The two local types mirror the imported shapes so TypeScript can
// type-check the call sites without reaching back into the cyclic
// modules.
type DisposeTerminalFn = (nodeId: number) => void;
type AgentNodeStoreModule = typeof import('./agentNodeStore');

// `Mesh` is generated from the Rust `models::Mesh` struct (issue #359).
// Re-exported here so the many `import { Mesh } from '../stores/meshStore'`
// call sites keep working. The generated type is the full 14-field wire shape,
// not the 6-field subset this store used to hand-declare.
export type { Mesh };

interface MeshState {
  meshes: Mesh[];
  meshesById: Map<number, Mesh>;
  selectedMeshId: number | null;
  loading: boolean;
  error: string | null;
  fetchMeshes: () => Promise<void>;
  addMesh: () => Promise<void>;
  addTestMesh: (name: string) => Promise<Mesh | null>;
  createMesh: (name: string, path: string, color?: string | null) => Promise<Mesh | null>;
  // Issue #1247 — returns `true` on successful delete + cleanup so callers
  // can gate their own follow-up (Probe close, navigation, toast) on the
  // real outcome instead of catching an error `deleteMesh` never throws.
  // Errors continue to be surfaced via `state.error` to match the rest of
  // the meshStore surface.
  deleteMesh: (id: number) => Promise<boolean>;
  updateMeshColor: (id: number, color: string | null) => Promise<void>;
  selectMesh: (id: number | null) => void;
  updateMeshLayout: (id: number, layout: 'grid' | 'single') => Promise<void>;
  reorderMeshes: (meshId: number, newPosition: number) => Promise<void>;
  updateMeshName: (id: number, name: string) => Promise<void>;
  getDefaultProvider: (meshId: number) => Promise<string>;
}

export const useMeshStore = create<MeshState>((set) => ({
  meshes: [],
  meshesById: new Map(),
  selectedMeshId: null,
  loading: false,
  error: null,

  fetchMeshes: async () => {
    set({ loading: true, error: null });
    try {
      const meshes = await api.listMeshes();
      const meshesById = new Map(meshes.map((p) => [p.id, p]));
      set({ meshes, meshesById, loading: false });
    } catch (e) {
      set({ error: formatError(e), loading: false });
    }
  },

  addMesh: async () => {
    try {
      const mesh = await api.addMesh();
      set((state) => ({
        meshes: [...state.meshes, mesh],
        meshesById: new Map([...state.meshesById, [mesh.id, mesh]])
      }));
    } catch (e) {
      set({ error: formatError(e) });
    }
  },

  addTestMesh: async (name) => {
    try {
      const mesh = await api.createTestMesh(name);
      set((state) => ({
        meshes: [...state.meshes, mesh],
        meshesById: new Map([...state.meshesById, [mesh.id, mesh]])
      }));
      return mesh;
    } catch (e) {
      set({ error: formatError(e) });
      return null;
    }
  },

  createMesh: async (name, path, color) => {
    try {
      const mesh = await api.createMesh(name, path, color);
      set((state) => ({
        meshes: [...state.meshes, mesh],
        meshesById: new Map([...state.meshesById, [mesh.id, mesh]]),
      }));
      return mesh;
    } catch (e) {
      set({ error: formatError(e) });
      return null;
    }
  },

  deleteMesh: async (id): Promise<boolean> => {
    // Issue #1247 — capture doomed node ids BEFORE the backend delete
    // commits. Pre-#1247 the function only refetched meshes, which left:
    //   * ghost `agentNodes` rows still clickable in the grid + sidebar —
    //     clicking × fired `delete_agent_node` on a vanished row and
    //     surfaced a spurious error toast;
    //   * xterm instances + 'agent-output' listeners alive in
    //     `TerminalRegistry.instances` (the terminal-persistence rule is
    //     satisfied because the node IS deleted — `disposeTerminal` is
    //     the same dispose-on-delete site `agentNodeStore.deleteAgentNode`
    //     uses on its success path);
    //   * `selectedMeshId` / `activeNodeId` dangling into the deleted
    //     mesh — the Probe rendered against
    //     `meshesById.get(danglingId) === undefined`, and `uiStore`'s
    //     mesh-sync subscription never fired because `selectedMeshId`
    //     was never nulled.
    //
    // Lazy-import the agent-node store to avoid the
    // meshStore → agentNodeStore → Terminal → uiStore → meshStore
    // ESM cycle (see the import comment at the top of this file).
    // The capture is the FIRST thing the method does so a doomed-id
    // capture failure (highly unlikely, but the IPC below is what
    // really fails) can't strand the dispose loop without its target
    // list. The `await` is the same one a static import would have
    // hidden — `deleteMesh` is already async.
    const agentNodeModule = await import('./agentNodeStore') as AgentNodeStoreModule;
    const { useAgentNodeStore } = agentNodeModule;
    const agentStoreBefore = useAgentNodeStore.getState();
    const doomedNodeIds = agentStoreBefore.agentNodes
      .filter((n) => n.mesh_id === id)
      .map((n) => n.id);

    try {
      await api.deleteMesh(id);
    } catch (e) {
      set({ error: formatError(e) });
      return false;
    }

    // Backend committed — fan out the post-delete cleanup. Refetch
    // BEFORE disposing so the agent-node store's invariant ("never
    // dispose unless the row is deleted", issue #647) holds when we
    // call `disposeTerminal` below: the refetch just observed the row
    // is gone, and we know for sure the doomed ids are now DB-dead.
    await useMeshStore.getState().fetchMeshes();
    await useAgentNodeStore.getState().fetchAgentNodes();

    // One dispose per doomed node. `disposeTerminal` is a no-op for
    // node ids that were never mounted (the registry's `instances.get`
    // returns `undefined`), so a doomed id that never opened a
    // terminal still gets a free, idempotent call — matches the
    // deleteAgentNode success path's "always dispose, the registry
    // absorbs the no-op" discipline.
    //
    // Lazy-imported to break the cycle (see the import comment).
    if (doomedNodeIds.length > 0) {
      const terminalModule = await import('../components/Terminal/Terminal') as {
        disposeTerminal: DisposeTerminalFn;
      };
      for (const nodeId of doomedNodeIds) {
        terminalModule.disposeTerminal(nodeId);
      }
    }

    // Clear dangling selection slots. Re-read AFTER the refetches so
    // a mesh-switch race (user picks a different mesh mid-await) does
    // NOT clobber a fresh `selectedMeshId` — only null the slot if it
    // STILL points into the deleted mesh after the cleanup. Same race
    // guard for `activeNodeId`: if the user clicked a different node
    // mid-await, that fresh active id is in a different mesh and we
    // leave it alone.
    const meshAfter = useMeshStore.getState();
    if (meshAfter.selectedMeshId === id) {
      meshAfter.selectMesh(null);
    }
    const agentAfter = useAgentNodeStore.getState();
    if (agentAfter.activeNodeId !== null && doomedNodeIds.includes(agentAfter.activeNodeId)) {
      agentAfter.setActiveNode(null);
    }

    return true;
  },

  selectMesh: (id) => set({ selectedMeshId: id }),

  updateMeshLayout: async (id, layout) => {
    try {
      await api.updateMeshLayout(id, layout);
      set((state) => {
        const existing = state.meshesById.get(id);
        if (!existing) return state;
        const updated = { ...existing, layout };
        return {
          meshes: state.meshes.map((p) => (p.id === id ? updated : p)),
          meshesById: new Map([...state.meshesById, [id, updated]])
        };
      });
    } catch (e) {
      set({ error: formatError(e) });
    }
  },

  reorderMeshes: async (meshId, newPosition) => {
    let updatedMeshes: Mesh[];
    set((state) => {
      const meshes = [...state.meshes];
      const draggedIdx = meshes.findIndex((p) => p.id === meshId);
      if (draggedIdx === -1) return state;
      const [dragged] = meshes.splice(draggedIdx, 1);
      meshes.splice(newPosition, 0, dragged);
      updatedMeshes = meshes.map((p, idx) => ({ ...p, position: idx }));
      const meshesById = new Map(updatedMeshes.map((p) => [p.id, p]));
      return { meshes: updatedMeshes, meshesById };
    });
    try {
      // Send ALL meshes' positions so DB stays in sync with optimistic update
      const currentMeshes = updatedMeshes!;
      const updates = currentMeshes.map((p) => [p.id, p.position] as [number, number]);
      await api.updateMeshPositions(updates);
    } catch (e) {
      set({ error: formatError(e) });
      await useMeshStore.getState().fetchMeshes();
    }
  },

  updateMeshName: async (id, name) => {
    try {
      await api.updateMeshName(id, name);
      set((state) => {
        const existing = state.meshesById.get(id);
        if (!existing) return state;
        const updated = { ...existing, name };
        return {
          meshes: state.meshes.map((m) => (m.id === id ? updated : m)),
          meshesById: new Map([...state.meshesById, [id, updated]])
        };
      });
    } catch (e) {
      set({ error: formatError(e) });
    }
  },

  updateMeshColor: async (id, color) => {
    try {
      await api.updateMeshColor(id, color);
      set((state) => {
        const existing = state.meshesById.get(id);
        if (!existing) return state;
        const updated = { ...existing, color };
        return {
          meshes: state.meshes.map((m) => (m.id === id ? updated : m)),
          meshesById: new Map([...state.meshesById, [id, updated]]),
        };
      });
    } catch (e) {
      set({ error: formatError(e) });
    }
  },

  getDefaultProvider: async (meshId) => {
    try {
      return await api.getDefaultProvider(meshId);
    } catch (e) {
      set({ error: formatError(e) });
      // Post-#538 the native Claude harness profile id is `'claude'`
      // (the unified-claude-harness change renamed the bare `'anthropic'`
      // provider id to the new harness-profile form). The backend's
      // `get_default_provider` IPC returns the post-#538 form, so the
      // fallback should mirror it for symmetry.
      return 'claude';
    }
  },
}));
