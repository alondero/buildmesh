import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen, fireEvent, waitFor, within } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { CloneActivityEntry } from '../../src/components/Mesh/CloneActivityEntry';
import { CloneProgressModal } from '../../src/components/Mesh/CloneProgressModal';
import { useCloneActivityStore } from '../../src/stores/cloneActivityStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useToastStore } from '../../src/stores/toastStore';
import { useUIStore } from '../../src/stores/uiStore';

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

const TARGET = {
  repo: 'alondero/buildmesh',
  parentDir: 'D:\\repos',
  destination: 'D:\\repos\\buildmesh',
  color: '#ff0000',
};

function resetStores() {
  useCloneActivityStore.setState({ operations: [], claimedOperationCounts: {} });
  useMeshStore.setState({
    meshes: [],
    meshesById: new Map(),
    selectedMeshId: null,
    loading: false,
    error: null,
  });
  useToastStore.setState({ toasts: [] });
  useUIStore.setState({ cloneProgressOperationId: null });
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

/** The activity entry and the progress surface, wired the way App wires them. */
function ActivityHarness() {
  const operationId = useUIStore((s) => s.cloneProgressOperationId);
  return (
    <>
      <CloneActivityEntry />
      {operationId !== null && (
        <CloneProgressModal
          operationId={operationId}
          onClose={() => useUIStore.getState().closeCloneProgress()}
        />
      )}
    </>
  );
}

describe('clone activity entry and progress surface', () => {
  beforeEach(() => {
    resetStores();
    // `vi.clearAllMocks()` (the global setup) does NOT clear queued
    // `mockReturnValueOnce` values, so a leftover from one test would answer
    // the next test's call instead. `mockReset()` does.
    mockInvoke.mockReset();
  });

  it('keeps the running clone visible with a route back to its progress', async () => {
    pendingClone();
    const operation = useCloneActivityStore.getState().startClone(TARGET).operation;

    render(<ActivityHarness />);

    // The entry names the operation and says it is still running.
    const entry = screen.getByTestId('clone-activity-entry');
    expect(entry.textContent).toContain('Cloning');
    expect(entry.textContent).toContain('alondero/buildmesh');
    // Nothing offers to cancel it.
    expect(entry.textContent).not.toMatch(/cancel/i);

    // Clicking it opens the progress for THAT operation.
    fireEvent.click(screen.getByRole('button', { name: /open clone details/i }));
    expect(useUIStore.getState().cloneProgressOperationId).toBe(operation.id);
    expect(await screen.findByRole('dialog')).toBeTruthy();
    expect(screen.getByText('alondero/buildmesh')).toBeTruthy();
  });

  it('shows the submitted target and an elapsed time, and invents no percentage', async () => {
    pendingClone();
    useCloneActivityStore.getState().startClone(TARGET);

    render(
      <CloneProgressModal
        operationId={useCloneActivityStore.getState().operations[0].id}
        onClose={() => {}}
      />
    );

    // The submitted repository and destination — not a later form value.
    expect(screen.getByText('alondero/buildmesh')).toBeTruthy();
    expect(screen.getByText('D:\\repos\\buildmesh')).toBeTruthy();
    expect(screen.getByTestId('clone-progress-elapsed')).toBeTruthy();

    // The progress bar is indeterminate: the backend reports no transfer
    // progress, so the surface must not claim a percentage.
    const bar = screen.getByRole('progressbar');
    expect(bar.getAttribute('aria-valuenow')).toBeNull();
    expect(bar.textContent).toBe('');

    // The dismissal says what it does.
    expect(screen.getAllByRole('button', { name: /hide progress/i }).length).toBe(2);
    expect(screen.queryByRole('button', { name: /^cancel$/i })).toBeNull();
  });

  it('reports how long a settled clone took, not how long ago it started', () => {
    // A clone that ran for five seconds and is opened an hour later took
    // five seconds: the duration is measured to the moment it settled.
    const startedAt = Date.now() - 3_600_000;
    const finishedAt = startedAt + 5_000;
    useCloneActivityStore.setState(state => ({
      operations: [
        {
          ...TARGET,
          id: 'clone-settled',
          repoKey: 'alondero/buildmesh',
          status: 'succeeded',
          startedAt,
          finishedAt,
          meshId: CLONED_MESH.id,
          meshName: CLONED_MESH.name,
          error: null,
        },
      ],
    }));

    render(
      <CloneProgressModal
        operationId="clone-settled"
        onClose={() => {}}
      />
    );

    expect(screen.getByTestId('clone-progress-elapsed').textContent).toBe('5s');
  });

  it('opens the mesh a successful clone registered', async () => {
    mockInvoke.mockResolvedValueOnce(CLONED_MESH);
    const operation = useCloneActivityStore.getState().startClone(TARGET).operation;

    render(
      <CloneProgressModal
        operationId={operation.id}
        onClose={() => {}}
      />
    );

    await waitFor(() => expect(screen.getByRole('button', { name: /open mesh/i })).toBeTruthy());
    fireEvent.click(screen.getByRole('button', { name: /open mesh/i }));

    expect(useMeshStore.getState().selectedMeshId).toBe(CLONED_MESH.id);
  });

  it('opens the mesh straight from the activity entry', async () => {
    mockInvoke.mockResolvedValueOnce(CLONED_MESH);
    useCloneActivityStore.getState().startClone(TARGET);
    await waitFor(() =>
      expect(useCloneActivityStore.getState().operations[0].status).toBe('succeeded')
    );

    render(<ActivityHarness />);

    fireEvent.click(screen.getByRole('button', { name: /open mesh/i }));

    expect(useMeshStore.getState().selectedMeshId).toBe(CLONED_MESH.id);
  });

  it('keeps a failure recoverable after its toast has expired', async () => {
    const { rejectClone } = pendingClone();
    useCloneActivityStore.getState().startClone(TARGET);

    render(<ActivityHarness />);
    rejectClone(new Error('git clone failed: Repository not found'));

    await waitFor(() =>
      expect(useCloneActivityStore.getState().operations[0].status).toBe('failed')
    );
    // The toast is the transient half of the report…
    await waitFor(() => expect(useToastStore.getState().toasts).toHaveLength(1));

    // …and the entry is the durable half. Expire the toast: the failure is
    // still there, with its error and a way to retry it.
    useToastStore.getState().dismissToast(useToastStore.getState().toasts[0].id);
    expect(useToastStore.getState().toasts).toHaveLength(0);

    const entry = screen.getByTestId('clone-activity-entry');
    expect(entry.textContent).toContain('Clone failed');
    fireEvent.click(screen.getByRole('button', { name: /open clone details/i }));

    expect(await screen.findByText(/Repository not found/)).toBeTruthy();
    expect(screen.getByRole('button', { name: /retry/i })).toBeTruthy();
  });

  it('retries the submitted target after a failure', async () => {
    const { rejectClone } = pendingClone();
    const operation = useCloneActivityStore.getState().startClone(TARGET).operation;

    render(<ActivityHarness />);
    rejectClone(new Error('git clone failed: Repository not found'));
    await waitFor(() =>
      expect(useCloneActivityStore.getState().operations[0].status).toBe('failed')
    );

    fireEvent.click(screen.getByRole('button', { name: /open clone details/i }));
    mockInvoke.mockResolvedValueOnce(CLONED_MESH);
    fireEvent.click(await screen.findByRole('button', { name: /retry/i }));

    // The retry clones the SAME repository into the SAME destination.
    await waitFor(() =>
      expect(mockInvoke).toHaveBeenLastCalledWith('clone_mesh_repo', {
        url: TARGET.repo,
        parentDir: TARGET.parentDir,
        color: TARGET.color,
      })
    );
    // …and it is a new operation, not the failed one resurrected.
    await waitFor(() =>
      expect(useCloneActivityStore.getState().operations).toHaveLength(2)
    );
    const retry = useCloneActivityStore.getState().operations[1];
    expect(retry.id).not.toBe(operation.id);
    expect(retry.repo).toBe(TARGET.repo);
    expect(retry.destination).toBe(TARGET.destination);
  });

  it('lets the user dismiss a settled entry', async () => {
    mockInvoke.mockResolvedValueOnce(CLONED_MESH);
    useCloneActivityStore.getState().startClone(TARGET);
    await waitFor(() =>
      expect(useCloneActivityStore.getState().operations[0].status).toBe('succeeded')
    );

    render(<ActivityHarness />);
    expect(screen.getByTestId('clone-activity-entry')).toBeTruthy();

    fireEvent.click(
      screen.getByRole('button', { name: /dismiss clone activity for/i })
    );

    expect(screen.queryByTestId('clone-activity-entry')).toBeNull();
    expect(useCloneActivityStore.getState().operations).toHaveLength(0);
  });

  it('gives every running clone its own entry and its own route to progress', async () => {
    pendingClone();
    mockInvoke.mockReturnValueOnce(new Promise(() => {}));
    const first = useCloneActivityStore.getState().startClone(TARGET).operation;
    const second = useCloneActivityStore
      .getState()
      .startClone({ ...TARGET, repo: 'alondero/other', destination: 'D:\\repos\\other' })
      .operation;

    render(<ActivityHarness />);

    // Both clones are visible: a second clone with no entry of its own would
    // be invisible while it runs.
    const entries = screen.getAllByTestId('clone-activity-entry');
    expect(entries).toHaveLength(2);
    expect(entries[0].textContent).toContain('alondero/buildmesh');
    expect(entries[1].textContent).toContain('alondero/other');

    // Each entry routes to ITS OWN operation.
    fireEvent.click(within(entries[1]).getByRole('button', { name: /open clone details/i }));
    expect(useUIStore.getState().cloneProgressOperationId).toBe(second.id);
    fireEvent.click(
      within(entries[0]).getByRole('button', { name: /open clone details/i })
    );
    expect(useUIStore.getState().cloneProgressOperationId).toBe(first.id);
  });
});
