import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { MeshCreateModal } from '../../src/components/Mesh/MeshCreateModal';
import { useMeshStore } from '../../src/stores/meshStore';
import { useToastStore } from '../../src/stores/toastStore';

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

/** Switch to clone mode, enter a repo, and pick a parent folder so the form is
 * submittable. Leaves the dialog on the clone tab. */
async function fillCloneForm(repo = 'alondero/buildmesh') {
  fireEvent.click(screen.getByRole('button', { name: /clone from github/i }));
  fireEvent.change(screen.getByLabelText('Repository'), { target: { value: repo } });
  fireEvent.click(screen.getByRole('button', { name: /choose parent folder/i }));
  // The picker relabels once a folder has been chosen.
  await waitFor(() =>
    expect(screen.getByRole('button', { name: /change folder/i })).toBeTruthy()
  );
}

describe('MeshCreateModal', () => {
  beforeEach(() => {
    resetMeshStore();
    useToastStore.setState({ toasts: [] });
  });

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

  it('locks the clone form controls while the clone is in flight', async () => {
    mockInvoke.mockResolvedValueOnce({ path: 'D:\\repos', name: 'repos' }); // pick
    let resolveClone: (value: unknown) => void = () => {};
    mockInvoke.mockReturnValueOnce(
      new Promise((resolve) => {
        resolveClone = resolve;
      })
    ); // clone_mesh_repo, held open

    render(<MeshCreateModal onClose={() => {}} />);
    await fillCloneForm();
    fireEvent.click(screen.getByRole('button', { name: /^clone mesh$/i }));

    // While the clone is unresolved the submit control reports pending + disabled.
    const pending = (await screen.findByRole('button', {
      name: /cloning/i,
    })) as HTMLButtonElement;
    expect(pending.disabled).toBe(true);

    // Everything that could retarget the clone, or change which tab it reports
    // into, is locked for the duration.
    expect((screen.getByLabelText('Repository') as HTMLInputElement).disabled).toBe(true);
    expect(
      (screen.getByRole('button', { name: /change folder/i }) as HTMLButtonElement).disabled
    ).toBe(true);
    expect(
      (screen.getByRole('button', { name: /open folder/i }) as HTMLButtonElement).disabled
    ).toBe(true);
    expect(
      (screen.getByRole('button', { name: /clone from github/i }) as HTMLButtonElement).disabled
    ).toBe(true);
    // Cancel deliberately stays available: dismissal is safe (mount-guarded
    // completion + toast) and trapping the user for the clone timeout would not be.
    expect((screen.getByRole('button', { name: /^cancel$/i }) as HTMLButtonElement).disabled).toBe(
      false
    );

    resolveClone(CLONED_MESH);
    await waitFor(() => expect(useMeshStore.getState().meshes).toHaveLength(1));
  });

  it('reports a clone that finishes after the dialog was dismissed', async () => {
    mockInvoke.mockResolvedValueOnce({ path: 'D:\\repos', name: 'repos' });
    let resolveClone: (value: unknown) => void = () => {};
    mockInvoke.mockReturnValueOnce(
      new Promise((resolve) => {
        resolveClone = resolve;
      })
    );

    const { unmount } = render(<MeshCreateModal onClose={() => {}} />);
    await fillCloneForm();
    fireEvent.click(screen.getByRole('button', { name: /^clone mesh$/i }));
    await screen.findByRole('button', { name: /cloning/i });

    unmount();
    resolveClone(CLONED_MESH);

    // The mesh is real — the backend already committed it — so the outcome is
    // reported rather than silently appearing in the sidebar.
    await waitFor(() =>
      expect(useToastStore.getState().toasts.some((t) => t.provider === 'Clone')).toBe(true)
    );
    expect(useToastStore.getState().toasts[0].severity).toBe('success');
    expect(useMeshStore.getState().meshes).toHaveLength(1);
  });

  it('reports a clone failure that lands after the dialog was dismissed', async () => {
    mockInvoke.mockResolvedValueOnce({ path: 'D:\\repos', name: 'repos' });
    let rejectClone: (reason: unknown) => void = () => {};
    mockInvoke.mockReturnValueOnce(
      new Promise((_resolve, reject) => {
        rejectClone = reject;
      })
    );

    const { unmount } = render(<MeshCreateModal onClose={() => {}} />);
    await fillCloneForm('nope/nope');
    fireEvent.click(screen.getByRole('button', { name: /^clone mesh$/i }));
    await screen.findByRole('button', { name: /cloning/i });

    unmount();
    rejectClone(new Error('git clone failed: Repository not found'));

    await waitFor(() => {
      const toasts = useToastStore.getState().toasts;
      expect(toasts.some((t) => /Repository not found/.test(t.message))).toBe(true);
      expect(toasts[0].severity).toBe('error');
    });
  });

  it('submits the clone form with Enter from the repository field', async () => {
    mockInvoke
      .mockResolvedValueOnce({ path: 'D:\\repos', name: 'repos' })
      .mockResolvedValueOnce(CLONED_MESH);

    render(<MeshCreateModal onClose={() => {}} />);
    await fillCloneForm();

    fireEvent.keyDown(screen.getByLabelText('Repository'), { key: 'Enter' });

    await waitFor(() =>
      expect(mockInvoke).toHaveBeenCalledWith('clone_mesh_repo', {
        url: 'alondero/buildmesh',
        parentDir: 'D:\\repos',
        color: expect.any(String),
      })
    );
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
