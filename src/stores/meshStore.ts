import { create } from 'zustand';
import * as api from '../lib/tauri';
import type { Mesh } from '../types/generated/Mesh';

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
  createMesh: (name: string, path: string) => Promise<void>;
  deleteMesh: (id: number) => Promise<void>;
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
      set({ error: String(e), loading: false });
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
      set({ error: String(e) });
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
      set({ error: String(e) });
      return null;
    }
  },

  createMesh: async (name, path) => {
    try {
      await api.createMesh(name, path);
      await useMeshStore.getState().fetchMeshes();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  deleteMesh: async (id) => {
    try {
      await api.deleteMesh(id);
      await useMeshStore.getState().fetchMeshes();
    } catch (e) {
      set({ error: String(e) });
    }
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
      set({ error: String(e) });
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
      set({ error: String(e) });
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
      set({ error: String(e) });
    }
  },

  getDefaultProvider: async (meshId) => {
    try {
      return await api.getDefaultProvider(meshId);
    } catch (e) {
      set({ error: String(e) });
      return 'anthropic';
    }
  },
}));