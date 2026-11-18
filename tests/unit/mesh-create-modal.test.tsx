import { describe, it, expect, beforeEach, vi } from 'vitest';
import { StrictMode, useState } from 'react';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { MeshCreateModal } from '../../src/components/Mesh/MeshCreateModal';
import { CloneProgressModal } from '../../src/components/Mesh/CloneProgressModal';
import { useMeshStore } from '../../src/stores/meshStore';
import { useToastStore } from '../../src/stores/toastStore';
import { useCloneActivityStore } from '../../src/stores/cloneActivityStore';

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

/**
 * The clone-activity store owns clone operations for the whole session, so
 * it survives a test the way it survives a dialog. Each test starts with an
 * empty one — otherwise a clone left running by an earlier test would join
 * the next test's submission instead of starting its own.
 */
function resetCloneActivity() {
  useCloneActivityStore.setState({ operations: [], claimedOperationCounts: {} });
}

/** Switch to clone mode, enter a repo, and pick a parent folder so the form is
 *  submittable. Leaves the dialog on the clone tab. */
async function fillCloneForm(repo = 'alondero/buildmesh') {
  fireEvent.click(screen.getByRole('button', { name: /clone from github/i }));
  fireEvent.change(screen.getByLabelText('Repository'), { target: { value: repo } });
  fireEvent.click(screen.getByRole('button', { name: /choose parent folder/i }));
  // The picker relabels once a folder has been chosen.
  await waitFor(() =>
    expect(screen.getByRole('button', { name: /change folder/i })).toBeTruthy()
  );
}

/** A clone IPC that stays pending until the test resolves it. */
function pendingClone() {
  let resolveClone: (value: unknown) => void = () => {};
  let rejectClone: (reason: unknown) => void = () => {};
  const promise = new Promise((resolve, reject) => {
    resolveClone = resolve;
    rejectClone = reject;
  });
  mockInvoke.mockReturnValueOnce(promise);
  return { resolveClone, rejectClone };
}

/** The dialog mounted the way the app mounts it: closing unmounts it. */
function MountedModal({ onClose }: { onClose?: () => void }) {
  const [open, setOpen] = useState(true);
  if (!open) return null;
  return (
    <MeshCreateModal
      onClose={() => {
        onClose?.();
        setOpen(false);
      }}
    />
  );
}

describe('MeshCreateModal', () => {
  beforeEach(() => {
    resetMeshStore();
    resetCloneActivity();
    useToastStore.setState({ toasts: [] });
    // `vi.clearAllMocks()` (the global setup) does NOT clear queued
    // `mockReturnValueOnce` values, so a leftover from one test would answer
    // the next test's call instead. `mockReset()` does.
    mockInvoke.mockReset();
  });

  it('starts in open-folder mode', () => {
    render(<MeshCreateModal onClose={() => {}} />);
    expect(screen.getByRole('button', { name: /choose folder/i })).toBeTruthy();
    expect(screen.queryByLabelText('Repository')).toBeNull();
  });

  it('moves focus inside the dialog to the first required step on open (issue #2463)', () => {
    // Reproduces the reported path: before the fix, opening the dialog left
    // focus on the sidebar trigger, because the submit button the modal
    // targeted is disabled until a folder is picked — and `.focus()` on a
    // disabled button is a silent no-op.
    const trigger = document.createElement('button');
    trigger.textContent = '+ New mesh';
    document.body.appendChild(trigger);
    trigger.focus();
    expect(document.activeElement).toBe(trigger);

    render(<MeshCreateModal onClose={() => {}} />);

    const dialog = screen.getByRole('dialog');
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /choose folder/i }));
    expect(dialog.contains(document.activeElement)).toBe(true);
    trigger.remove();
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

  it.each(['open', 'clone'])('completes %s after Strict Mode replays mount effects', async source => {
    mockInvoke
      .mockResolvedValueOnce({ path: '/repos', name: 'repos' })
      .mockResolvedValueOnce(CLONED_MESH);
    const onClose = vi.fn();
    render(<StrictMode><MeshCreateModal onClose={onClose} /></StrictMode>);
    if (source === 'clone') await fillCloneForm();
    else {
      fireEvent.click(screen.getByRole('button', { name: /choose folder/i }));
      await screen.findByText('/repos');
    }
    fireEvent.click(screen.getByRole('button', { name: source === 'clone' ? /^clone mesh$/i : /^create mesh$/i }));
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    expect(useMeshStore.getState().selectedMeshId).toBe(CLONED_MESH.id);
    expect(useToastStore.getState().toasts).toHaveLength(0);
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
    const { resolveClone } = pendingClone(); // clone_mesh_repo, held open

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

    resolveClone(CLONED_MESH);
    await waitFor(() => expect(useMeshStore.getState().meshes).toHaveLength(1));
  });

  // ---- Issue #2795: hiding progress is not cancelling ------------------------

  it('starts no operation when Cancel is pressed before the clone is submitted', async () => {
    mockInvoke.mockResolvedValueOnce({ path: 'D:\\repos', name: 'repos' }); // pick

    render(<MeshCreateModal onClose={() => {}} />);
    await fillCloneForm();

    fireEvent.click(screen.getByRole('button', { name: /^cancel$/i }));

    // Nothing was cloned, and no operation was recorded.
    expect(useCloneActivityStore.getState().operations).toHaveLength(0);
    expect(
      mockInvoke.mock.calls.filter(([cmd]) => cmd === 'clone_mesh_repo')
    ).toHaveLength(0);
  });

  it('renames the busy dismissal to Hide progress and never claims to cancel', async () => {
    mockInvoke.mockResolvedValueOnce({ path: 'D:\\repos', name: 'repos' }); // pick
    pendingClone();

    render(<MeshCreateModal onClose={() => {}} />);
    await fillCloneForm();
    fireEvent.click(screen.getByRole('button', { name: /^clone mesh$/i }));
    await screen.findByRole('button', { name: /cloning/i });

    // The dismissal no longer says "Cancel" anywhere: the button, the header
    // close control, and the visible explanation all say the same thing.
    expect(screen.queryByRole('button', { name: /^cancel$/i })).toBeNull();
    const hideButtons = screen.getAllByRole('button', { name: /hide progress/i });
    // The footer button and the header close control.
    expect(hideButtons.length).toBe(2);
    expect((hideButtons[0] as HTMLButtonElement).disabled).toBe(false);
    expect(screen.getByText(/continues in the background/i)).toBeTruthy();
    expect(screen.getByText(/does not stop the clone/i)).toBeTruthy();
  });

  it('Escape hides the progress without stopping the clone', async () => {
    mockInvoke.mockResolvedValueOnce({ path: 'D:\\repos', name: 'repos' }); // pick
    const { resolveClone } = pendingClone();
    const onClose = vi.fn();

    render(<MountedModal onClose={onClose} />);
    await fillCloneForm();
    fireEvent.click(screen.getByRole('button', { name: /^clone mesh$/i }));
    await screen.findByRole('button', { name: /cloning/i });

    fireEvent.keyDown(document, { key: 'Escape' });

    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(screen.queryByRole('dialog')).toBeNull();

    // The clone is still running: dismissing the dialog did not cancel it.
    expect(useCloneActivityStore.getState().operations[0].status).toBe('running');

    resolveClone(CLONED_MESH);
    await waitFor(() => expect(useMeshStore.getState().meshes).toHaveLength(1));
  });

  it('the header close control hides the progress without stopping the clone', async () => {
    mockInvoke.mockResolvedValueOnce({ path: 'D:\\repos', name: 'repos' }); // pick
    const { resolveClone } = pendingClone();
    const onClose = vi.fn();

    render(<MountedModal onClose={onClose} />);
    await fillCloneForm();
    fireEvent.click(screen.getByRole('button', { name: /^clone mesh$/i }));
    await screen.findByRole('button', { name: /cloning/i });

    // The header close control is the first of the two "Hide progress"
    // controls (the header × and the footer button).
    const headerClose = screen.getAllByRole('button', { name: /hide progress/i })[0];
    fireEvent.click(headerClose);

    await waitFor(() => expect(onClose).toHaveBeenCalled());
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(useCloneActivityStore.getState().operations[0].status).toBe('running');

    resolveClone(CLONED_MESH);
    await waitFor(() => expect(useMeshStore.getState().meshes).toHaveLength(1));
  });

  it('keeps the dismissed clone in the activity store so it stays inspectable', async () => {
    mockInvoke.mockResolvedValueOnce({ path: 'D:\\repos', name: 'repos' }); // pick
    const { resolveClone } = pendingClone();

    const { unmount } = render(<MeshCreateModal onClose={() => {}} />);
    await fillCloneForm();
    fireEvent.click(screen.getByRole('button', { name: /^clone mesh$/i }));
    await screen.findByRole('button', { name: /cloning/i });

    unmount();
    resolveClone(CLONED_MESH);

    await waitFor(() =>
      expect(useCloneActivityStore.getState().operations[0].status).toBe('succeeded')
    );
    // The operation outlives the dialog — the submitted target is still on
    // record, so the user can be shown where the clone went.
    expect(useCloneActivityStore.getState().operations[0].destination).toBe(
      'D:\\repos\\buildmesh'
    );
    expect(useCloneActivityStore.getState().operations[0].meshId).toBe(CLONED_MESH.id);
  });

  it('reports a clone that finishes after the dialog was dismissed', async () => {
    mockInvoke.mockResolvedValueOnce({ path: 'D:\\repos', name: 'repos' });
    const { resolveClone } = pendingClone();

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
    const { rejectClone } = pendingClone();

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
    // The failure stays on record after its toast — the operation is the
    // durable half of the report.
    expect(useCloneActivityStore.getState().operations[0].status).toBe('failed');
  });

  it('joins a running clone instead of launching a second one', async () => {
    mockInvoke.mockResolvedValueOnce({ path: 'D:\\repos', name: 'repos' }); // pick
    const { resolveClone } = pendingClone(); // the first clone, held open

    const first = render(<MeshCreateModal onClose={() => {}} />);
    await fillCloneForm();
    fireEvent.click(screen.getByRole('button', { name: /^clone mesh$/i }));
    await screen.findByRole('button', { name: /cloning/i });
    first.unmount();

    // Reopen the dialog and submit the same repository into the same
    // destination while that clone is still running.
    mockInvoke.mockResolvedValueOnce({ path: 'D:\\repos', name: 'repos' }); // pick
    render(<MeshCreateModal onClose={() => {}} />);
    await fillCloneForm();
    fireEvent.click(screen.getByRole('button', { name: /^clone mesh$/i }));

    await waitFor(() =>
      expect(useCloneActivityStore.getState().operations[0].status).toBe('running')
    );
    // No second clone was launched — the submission joined the first.
    expect(
      mockInvoke.mock.calls.filter(([cmd]) => cmd === 'clone_mesh_repo')
    ).toHaveLength(1);
    expect(useCloneActivityStore.getState().operations).toHaveLength(1);
    // The reconciliation is visible, not silent: the dialog says the
    // submission joined a clone that was already running.
    expect(screen.getByText(/already running/i)).toBeTruthy();

    resolveClone(CLONED_MESH);
    await waitFor(() => expect(useMeshStore.getState().meshes).toHaveLength(1));
  });

  it('does not announce the outcome twice when the progress surface also shows it', async () => {
    mockInvoke.mockResolvedValueOnce({ path: 'D:\\repos', name: 'repos' }); // pick
    const { resolveClone } = pendingClone();

    const dialog = render(<MeshCreateModal onClose={() => {}} />);
    await fillCloneForm();
    fireEvent.click(screen.getByRole('button', { name: /^clone mesh$/i }));
    await screen.findByRole('button', { name: /cloning/i });

    // The progress surface opens for the same operation: two surfaces now
    // display it, so neither of them may have its claim revoked by the
    // other closing.
    const operationId = useCloneActivityStore.getState().operations[0].id;
    render(<CloneProgressModal operationId={operationId} onClose={() => {}} />);

    // The dialog goes away first. The progress surface is still showing the
    // operation, so the outcome is reported THERE — not as a second toast.
    dialog.unmount();
    resolveClone(CLONED_MESH);

    await waitFor(() =>
      expect(useCloneActivityStore.getState().operations[0].status).toBe('succeeded')
    );
    expect(useToastStore.getState().toasts).toHaveLength(0);
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

/**
 * Issue #2794 — a failed repository creation must show the backend's own
 * message and an inline recovery action.
 *
 * Before the fix the store recorded the failure in a shared error slot and
 * returned `null`, and the dialog replaced the operation-specific failure
 * with "Failed to create mesh. See logs for details." A new user could not
 * tell whether to choose another folder, fix access, or try again.
 */
describe('a failed repository creation (issue #2794)', () => {
  const ACCESS_DENIED = 'Access denied creating repository workspace';

  beforeEach(() => {
    resetMeshStore();
    useToastStore.setState({ toasts: [] });
    // Drop any IPC response left queued by an earlier test.
    mockInvoke.mockReset();
    mockInvoke.mockImplementation(async () => ({}));
  });

  /**
   * Route IPC by command name.
   *
   * The ordered `mockResolvedValueOnce` queue is not usable here: a rejected
   * command also makes the IPC chokepoint call `log_frontend`
   * (`src/lib/tauri/_invoke.ts`), which would consume the next queued
   * response and answer the wrong request. Routing by name keeps each
   * command's outcome independent of the others.
   */
  function routeInvoke(handlers: Record<string, () => unknown>) {
    mockInvoke.mockImplementation(async (cmd: string) => {
      const handler = handlers[cmd];
      return handler ? handler() : {};
    });
  }

  /** Pick a folder through the mocked native picker. */
  async function chooseFolder(path = '/repos/app', name = 'app') {
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'pick_mesh_folder' ? { path, name } : {}
    );
    fireEvent.click(screen.getByRole('button', { name: /choose folder|change folder/i }));
    await screen.findByText(path, { exact: true });
  }

  it('shows the backend failure instead of the generic logs message', async () => {
    render(<MeshCreateModal onClose={() => {}} />);
    await chooseFolder();
    routeInvoke({
      create_mesh: () => {
        throw new Error(ACCESS_DENIED);
      },
    });
    fireEvent.click(screen.getByRole('button', { name: /^create mesh$/i }));

    // The distinctive rejection reaches the dialog…
    await screen.findByText(ACCESS_DENIED);
    // …and the generic copy does not replace it.
    expect(screen.queryByText(/see logs for details/i)).toBeNull();
    expect(screen.queryByText(/failed to create mesh/i)).toBeNull();
  });

  it('offers another folder for an access failure', async () => {
    render(<MeshCreateModal onClose={() => {}} />);
    await chooseFolder();
    routeInvoke({
      create_mesh: () => {
        throw new Error(ACCESS_DENIED);
      },
    });
    fireEvent.click(screen.getByRole('button', { name: /^create mesh$/i }));
    await screen.findByText(ACCESS_DENIED);

    expect(screen.getByTestId('mesh-create-change-folder')).toBeTruthy();
    expect(screen.queryByTestId('mesh-create-retry')).toBeNull();
  });

  it('offers another folder for a missing folder', async () => {
    render(<MeshCreateModal onClose={() => {}} />);
    await chooseFolder();
    routeInvoke({
      create_mesh: () => {
        throw new Error('The system cannot find the path specified.');
      },
    });
    fireEvent.click(screen.getByRole('button', { name: /^create mesh$/i }));
    await screen.findByText('The system cannot find the path specified.');

    expect(screen.getByTestId('mesh-create-change-folder')).toBeTruthy();
    expect(screen.queryByTestId('mesh-create-retry')).toBeNull();
  });

  it('offers a retry for a transient failure', async () => {
    render(<MeshCreateModal onClose={() => {}} />);
    await chooseFolder();
    routeInvoke({
      create_mesh: () => {
        throw new Error('database is locked');
      },
    });
    fireEvent.click(screen.getByRole('button', { name: /^create mesh$/i }));
    await screen.findByText('database is locked');

    expect(screen.getByTestId('mesh-create-retry')).toBeTruthy();
    expect(screen.queryByTestId('mesh-create-change-folder')).toBeNull();
  });

  it('keeps the real message and both safe choices for an unclassified failure', async () => {
    render(<MeshCreateModal onClose={() => {}} />);
    await chooseFolder();
    routeInvoke({
      create_mesh: () => {
        throw new Error('the backend said something unexpected');
      },
    });
    fireEvent.click(screen.getByRole('button', { name: /^create mesh$/i }));
    await screen.findByText('the backend said something unexpected');

    expect(screen.getByTestId('mesh-create-retry')).toBeTruthy();
    expect(screen.getByTestId('mesh-create-change-folder')).toBeTruthy();
  });

  it('keeps the picked folder and the live controls, and retries into a single mesh', async () => {
    const onClose = vi.fn();

    render(<MeshCreateModal onClose={onClose} />);
    await chooseFolder();
    let attempt = 0;
    routeInvoke({
      create_mesh: () => {
        attempt += 1;
        if (attempt === 1) throw new Error('database is locked');
        return CLONED_MESH;
      },
    });
    fireEvent.click(screen.getByRole('button', { name: /^create mesh$/i }));
    await screen.findByText('database is locked');

    // The draft survives the failure: the folder is still the selected one…
    expect(screen.getByText('/repos/app')).toBeTruthy();
    // …and the controls are live again.
    const submit = screen.getByRole('button', { name: /^create mesh$/i }) as HTMLButtonElement;
    expect(submit.disabled).toBe(false);

    fireEvent.click(screen.getByTestId('mesh-create-retry'));

    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    // One retry, one mesh: the failed attempt registered no mesh.
    expect(useMeshStore.getState().meshes).toHaveLength(1);
    expect(useMeshStore.getState().selectedMeshId).toBe(CLONED_MESH.id);
  });

  it('shows its own request result when another store failure overlaps it', async () => {
    render(<MeshCreateModal onClose={() => {}} />);
    await chooseFolder();
    routeInvoke({
      create_mesh: () => {
        throw new Error(ACCESS_DENIED);
      },
      add_mesh: () => {
        throw new Error('another request failed');
      },
    });
    fireEvent.click(screen.getByRole('button', { name: /^create mesh$/i }));
    await screen.findByText(ACCESS_DENIED);

    // A different request fails afterwards and takes over the shared error
    // slot. The dialog must still report ITS OWN request's result.
    await useMeshStore.getState().addMesh();
    expect(useMeshStore.getState().error).toContain('another request failed');
    expect(screen.getByText(ACCESS_DENIED)).toBeTruthy();
    expect(screen.queryByText('another request failed')).toBeNull();
  });

  it('announces the failure accessibly and keeps focus in the dialog', async () => {
    render(<MeshCreateModal onClose={() => {}} />);
    await chooseFolder();
    routeInvoke({
      create_mesh: () => {
        throw new Error(ACCESS_DENIED);
      },
    });

    const submit = screen.getByRole('button', { name: /^create mesh$/i }) as HTMLButtonElement;
    submit.focus();
    fireEvent.click(submit);

    // The failure is announced as it appears, without a focus move.
    const alert = await screen.findByRole('alert');
    expect(alert.textContent).toContain(ACCESS_DENIED);

    // Focus never leaves the dialog.
    const dialog = screen.getByRole('dialog');
    expect(dialog.contains(document.activeElement)).toBe(true);
  });

  it('does not start a second creation while one is in flight', async () => {
    let resolveCreate: (value: unknown) => void = () => {};

    render(<MeshCreateModal onClose={() => {}} />);
    await chooseFolder();
    routeInvoke({
      create_mesh: () =>
        new Promise((resolve) => {
          resolveCreate = resolve;
        }),
    });

    const submit = screen.getByRole('button', { name: /^create mesh$/i }) as HTMLButtonElement;
    fireEvent.click(submit);
    fireEvent.click(submit);

    // One request, whatever the click rate: the in-flight guard refuses the
    // second submission, so the folder is never registered twice.
    await screen.findByRole('button', { name: /creating/i });
    expect(mockInvoke.mock.calls.filter((call) => call[0] === 'create_mesh')).toHaveLength(1);

    resolveCreate(CLONED_MESH);
    await waitFor(() => expect(useMeshStore.getState().meshes).toHaveLength(1));
  });

  it('keeps the diagnostics available in an expandable, copyable area', async () => {
    render(<MeshCreateModal onClose={() => {}} />);
    await chooseFolder();
    routeInvoke({
      create_mesh: () => {
        throw new Error(ACCESS_DENIED);
      },
    });
    fireEvent.click(screen.getByRole('button', { name: /^create mesh$/i }));
    await screen.findByText(ACCESS_DENIED);

    const detail = screen.getByTestId('mesh-create-failure-detail');
    expect(detail.textContent).toContain('Folder: /repos/app');
    expect(detail.textContent).toContain(`Cause: ${ACCESS_DENIED}`);
  });

  it('keeps the focus inside the dialog after choosing another folder', async () => {
    render(<MeshCreateModal onClose={() => {}} />);
    await chooseFolder();
    routeInvoke({
      create_mesh: () => {
        throw new Error(ACCESS_DENIED);
      },
    });
    fireEvent.click(screen.getByRole('button', { name: /^create mesh$/i }));
    await screen.findByText(ACCESS_DENIED);

    // The recovery reopens the native picker and takes a different folder.
    mockInvoke.mockImplementation(async (cmd: string) =>
      cmd === 'pick_mesh_folder' ? { path: '/repos/other', name: 'other' } : {}
    );
    fireEvent.click(screen.getByTestId('mesh-create-change-folder'));
    await screen.findByText('/repos/other', { exact: true });

    // The failure panel held the focus and is gone; the focus must not be
    // dropped on the page behind the dialog.
    const dialog = screen.getByRole('dialog');
    await waitFor(() => expect(dialog.contains(document.activeElement)).toBe(true));
  });

  it('names a cause even when the backend said nothing', async () => {
    render(<MeshCreateModal onClose={() => {}} />);
    await chooseFolder();
    routeInvoke({
      create_mesh: () => {
        // A rejection that carries no words at all.
        throw new Error('   ');
      },
    });
    fireEvent.click(screen.getByRole('button', { name: /^create mesh$/i }));

    // An empty banner would name no cause; the dialog says what happened.
    await screen.findByText('The mesh could not be created.');
  });

  it('does not report a copy that did not happen', async () => {
    render(<MeshCreateModal onClose={() => {}} />);
    await chooseFolder();
    routeInvoke({
      create_mesh: () => {
        throw new Error(ACCESS_DENIED);
      },
    });
    fireEvent.click(screen.getByRole('button', { name: /^create mesh$/i }));
    await screen.findByText(ACCESS_DENIED);

    // jsdom has no clipboard API, which is what an insecure window looks
    // like. The button must not claim the details were copied.
    fireEvent.click(screen.getByText('Details', { exact: true }));
    fireEvent.click(screen.getByRole('button', { name: /copy details/i }));

    await waitFor(() =>
      expect(
        useToastStore.getState().toasts.some((t) => /clipboard is not available/i.test(t.message))
      ).toBe(true)
    );
    expect(screen.queryByText('Copied')).toBeNull();
  });
});
