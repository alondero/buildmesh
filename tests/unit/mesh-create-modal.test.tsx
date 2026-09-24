import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { MeshCreateModal } from '../../src/components/Mesh/MeshCreateModal';
import { useMeshStore } from '../../src/stores/meshStore';

const mockInvoke = invoke as ReturnType<typeof vi.fn>;

const CLONED_MESH = {
  id: 7,
  name: 'buildmesh',
  path: 'D:\\repos\\buildmesh',
  layout: 'grid' as const,
  position: 0,
  created_at: '',
  base_ref: 'origin/main',
};

function resetMeshStore() {
  useMeshStore.setState({
    meshes: [],
    meshesById: new Map(),
    selectedMeshId: null,
    loading: false,
    error: null,
  });
}

describe('MeshCreateModal', () => {
  beforeEach(resetMeshStore);

  it('starts in open-folder mode', () => {
    render(<MeshCreateModal onClose={() => {}} />);
    expect(screen.getByRole('button', { name: /choose folder/i })).toBeTruthy();
    expect(screen.queryByLabelText('Repository')).toBeNull();
  });

  it('creates a mesh from a chosen folder in open mode', async () => {
    mockInvoke
      .mockResolvedValueOnce({ path: '/repos/app', name: 'app' }) // pick_mesh_folder
      .mockResolvedValueOnce({ ...CLONED_MESH, id: 9, name: 'app', path: '/repos/app' });
    const onClose = vi.fn();

    render(<MeshCreateModal onClose={onClose} />);
    fireEvent.click(screen.getByRole('button', { name: /choose folder/i }));
    await waitFor(() => expect(screen.getByText('/repos/app')).toBeTruthy());
    fireEvent.click(screen.getByRole('button', { name: /^create mesh$/i }));

    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(mockInvoke).toHaveBeenCalledWith('create_mesh', {
      name: 'app',
      path: '/repos/app',
      color: expect.any(String),
    });
  });

  it('gates the Clone button on both a repo and a parent folder', () => {
    render(<MeshCreateModal onClose={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: /clone from github/i }));

    const repoField = screen.getByLabelText('Repository');
    const cloneButton = screen.getByRole('button', { name: /^clone mesh$/i }) as HTMLButtonElement;
    expect(cloneButton.disabled).toBe(true);

    // A repo alone is not enough — a destination parent folder is required.
    fireEvent.change(repoField, { target: { value: 'alondero/buildmesh' } });
    expect(cloneButton.disabled).toBe(true);
  });

  it('clones into the previewed destination and selects the new mesh', async () => {
    mockInvoke
      .mockResolvedValueOnce({ path: 'D:\\repos', name: 'repos' }) // pick_mesh_folder
      .mockResolvedValueOnce(CLONED_MESH); // clone_mesh_repo
    const onClose = vi.fn();

    render(<MeshCreateModal onClose={onClose} />);
    fireEvent.click(screen.getByRole('button', { name: /clone from github/i }));
    fireEvent.change(screen.getByLabelText('Repository'), {
      target: { value: 'alondero/buildmesh' },
    });
    fireEvent.click(screen.getByRole('button', { name: /choose parent folder/i }));

    // The preview shows exactly where the clone will land.
    await waitFor(() => expect(screen.getByText('D:\\repos\\buildmesh')).toBeTruthy());

    fireEvent.click(screen.getByRole('button', { name: /^clone mesh$/i }));

    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(mockInvoke).toHaveBeenCalledWith('clone_mesh_repo', {
      url: 'alondero/buildmesh',
      parentDir: 'D:\\repos',
      color: expect.any(String),
    });
    expect(useMeshStore.getState().selectedMeshId).toBe(7);
    expect(useMeshStore.getState().meshes).toHaveLength(1);
  });

  it('surfaces the backend clone error inline', async () => {
    mockInvoke
      .mockResolvedValueOnce({ path: 'D:\\repos', name: 'repos' })
      .mockRejectedValueOnce(new Error('git clone failed: Repository not found'));

    render(<MeshCreateModal onClose={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: /clone from github/i }));
    fireEvent.change(screen.getByLabelText('Repository'), { target: { value: 'nope/nope' } });
    fireEvent.click(screen.getByRole('button', { name: /choose parent folder/i }));
    await waitFor(() => expect(screen.getByText('D:\\repos\\nope')).toBeTruthy());

    fireEvent.click(screen.getByRole('button', { name: /^clone mesh$/i }));

    await waitFor(() => expect(screen.getByText(/Repository not found/)).toBeTruthy());
  });

  it('shows the in-flight Cloning… state and disables the primary control', async () => {
    mockInvoke.mockResolvedValueOnce({ path: 'D:\\repos', name: 'repos' }); // pick
    let resolveClone: (value: unknown) => void = () => {};
    mockInvoke.mockReturnValueOnce(
      new Promise((resolve) => {
        resolveClone = resolve;
      })
    ); // clone_mesh_repo, held open

    render(<MeshCreateModal onClose={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: /clone from github/i }));
    fireEvent.change(screen.getByLabelText('Repository'), {
      target: { value: 'alondero/buildmesh' },
    });
    fireEvent.click(screen.getByRole('button', { name: /choose parent folder/i }));
    await waitFor(() => expect(screen.getByText('D:\\repos\\buildmesh')).toBeTruthy());

    fireEvent.click(screen.getByRole('button', { name: /^clone mesh$/i }));

    // While the clone is unresolved the submit control reports pending + disabled.
    const pending = (await screen.findByRole('button', {
      name: /cloning/i,
    })) as HTMLButtonElement;
    expect(pending.disabled).toBe(true);

    resolveClone(CLONED_MESH);
    await waitFor(() => expect(useMeshStore.getState().meshes).toHaveLength(1));
  });

  it('clears a stale clone error when switching source', async () => {
    mockInvoke
      .mockResolvedValueOnce({ path: 'D:\\repos', name: 'repos' })
      .mockRejectedValueOnce(new Error('git clone failed: Repository not found'));

    render(<MeshCreateModal onClose={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: /clone from github/i }));
    fireEvent.change(screen.getByLabelText('Repository'), { target: { value: 'nope/nope' } });
    fireEvent.click(screen.getByRole('button', { name: /choose parent folder/i }));
    await waitFor(() => expect(screen.getByText('D:\\repos\\nope')).toBeTruthy());
    fireEvent.click(screen.getByRole('button', { name: /^clone mesh$/i }));
    await waitFor(() => expect(screen.getByText(/Repository not found/)).toBeTruthy());

    // The clone error must not linger above the folder form it doesn't describe.
    fireEvent.click(screen.getByRole('button', { name: /open folder/i }));
    expect(screen.queryByText(/Repository not found/)).toBeNull();
  });
});
