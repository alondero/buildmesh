import { describe, it, expect, beforeEach, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { useMeshStore } from '../../src/stores/meshStore';

const mockInvoke = invoke as ReturnType<typeof vi.fn>;

describe('useMeshStore', () => {
  beforeEach(() => {
    useMeshStore.setState({
      meshes: [],
      meshesById: new Map(),
      selectedMeshId: null,
      loading: false,
      error: null,
    });
    vi.clearAllMocks();
  });

  describe('fetchMeshes', () => {
    it('populates meshes and meshesById on success', async () => {
      const meshes = [
        { id: 1, name: 'project-a', path: '/a', layout: 'grid', position: 0, created_at: '2026-01-01' },
        { id: 2, name: 'project-b', path: '/b', layout: 'grid', position: 1, created_at: '2026-01-02' },
      ];
      mockInvoke.mockResolvedValueOnce(meshes);

      await useMeshStore.getState().fetchMeshes();

      const state = useMeshStore.getState();
      expect(state.meshes).toEqual(meshes);
      expect(state.meshesById.get(1)?.name).toBe('project-a');
      expect(state.meshesById.get(2)?.name).toBe('project-b');
      expect(state.loading).toBe(false);
      expect(state.error).toBeNull();
    });

    it('sets error on failure', async () => {
      mockInvoke.mockRejectedValueOnce(new Error('DB connection failed'));

      await useMeshStore.getState().fetchMeshes();

      const state = useMeshStore.getState();
      expect(state.error).toContain('DB connection failed');
      expect(state.loading).toBe(false);
      expect(state.meshes).toEqual([]);
    });

    it('sets loading true during fetch', async () => {
      let resolvePromise: (value: unknown) => void;
      mockInvoke.mockReturnValueOnce(new Promise(r => { resolvePromise = r; }));

      const promise = useMeshStore.getState().fetchMeshes();
      expect(useMeshStore.getState().loading).toBe(true);

      resolvePromise!([]);
      await promise;
      expect(useMeshStore.getState().loading).toBe(false);
    });
  });

  describe('selectMesh', () => {
    it('sets selectedMeshId', () => {
      useMeshStore.getState().selectMesh(42);
      expect(useMeshStore.getState().selectedMeshId).toBe(42);
    });

    it('clears selection with null', () => {
      useMeshStore.getState().selectMesh(42);
      useMeshStore.getState().selectMesh(null);
      expect(useMeshStore.getState().selectedMeshId).toBeNull();
    });
  });

  describe('deleteMesh', () => {
    it('calls delete_project and refetches', async () => {
      mockInvoke
        .mockResolvedValueOnce(undefined) // delete_project
        .mockResolvedValueOnce([]); // list_projects (refetch)

      await useMeshStore.getState().deleteMesh(5);

      expect(mockInvoke).toHaveBeenCalledWith('delete_project', { projectId: 5 });
    });

    it('sets error if deletion fails', async () => {
      mockInvoke.mockRejectedValueOnce(new Error('Not found'));

      await useMeshStore.getState().deleteMesh(99);

      expect(useMeshStore.getState().error).toContain('Not found');
    });
  });

  describe('reorderMeshes', () => {
    it('reorders optimistically then persists', async () => {
      const meshes = [
        { id: 1, name: 'a', path: '/a', layout: 'grid' as const, position: 0, created_at: '' },
        { id: 2, name: 'b', path: '/b', layout: 'grid' as const, position: 1, created_at: '' },
        { id: 3, name: 'c', path: '/c', layout: 'grid' as const, position: 2, created_at: '' },
      ];
      useMeshStore.setState({
        meshes,
        meshesById: new Map(meshes.map(p => [p.id, p])),
      });
      mockInvoke.mockResolvedValueOnce(undefined); // update_project_positions

      await useMeshStore.getState().reorderMeshes(3, 0);

      const state = useMeshStore.getState();
      expect(state.meshes[0].id).toBe(3);
      expect(state.meshes[1].id).toBe(1);
      expect(state.meshes[2].id).toBe(2);
      expect(state.meshes[0].position).toBe(0);
      expect(state.meshes[1].position).toBe(1);
      expect(state.meshes[2].position).toBe(2);
    });

    it('refetches from backend on reorder failure to restore state', async () => {
      const meshes = [
        { id: 1, name: 'a', path: '/a', layout: 'grid' as const, position: 0, created_at: '' },
        { id: 2, name: 'b', path: '/b', layout: 'grid' as const, position: 1, created_at: '' },
      ];
      useMeshStore.setState({
        meshes,
        meshesById: new Map(meshes.map(p => [p.id, p])),
      });
      mockInvoke
        .mockRejectedValueOnce(new Error('Constraint violation')) // update_project_positions
        .mockResolvedValueOnce(meshes); // list_projects refetch restores original order

      await useMeshStore.getState().reorderMeshes(2, 0);

      // fetchMeshes is called as recovery, which calls list_projects
      expect(mockInvoke).toHaveBeenCalledWith('list_projects');
    });
  });

  describe('updateMeshLayout', () => {
    it('updates layout optimistically in meshes and meshesById', async () => {
      const meshes = [
        { id: 1, name: 'a', path: '/a', layout: 'grid' as const, position: 0, created_at: '' },
      ];
      useMeshStore.setState({
        meshes,
        meshesById: new Map(meshes.map(p => [p.id, p])),
      });
      mockInvoke.mockResolvedValueOnce(undefined);

      await useMeshStore.getState().updateMeshLayout(1, 'grid');

      const state = useMeshStore.getState();
      expect(state.meshes[0].layout).toBe('grid');
      expect(state.meshesById.get(1)?.layout).toBe('grid');
      expect(mockInvoke).toHaveBeenCalledWith('update_project_layout', { projectId: 1, layout: 'grid' });
    });

    it('ignores update for unknown mesh id', async () => {
      useMeshStore.setState({ meshes: [], meshesById: new Map() });
      mockInvoke.mockResolvedValueOnce(undefined);

      await useMeshStore.getState().updateMeshLayout(999, 'grid');

      expect(useMeshStore.getState().meshes).toEqual([]);
    });
  });
});
