import { describe, it, expect, beforeEach, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import {
  selectVisibleOperations,
  useCloneActivityStore,
} from '../../src/stores/cloneActivityStore';
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

describe('cloneActivityStore', () => {
  beforeEach(() => {
    resetStores();
    // `vi.clearAllMocks()` (the global setup) does NOT clear queued
    // `mockReturnValueOnce` values, so a leftover from one test would answer
    // the next test's call instead. `mockReset()` does.
    mockInvoke.mockReset();
  });

  it('records the submitted target and runs the clone', () => {
    mockInvoke.mockResolvedValueOnce(CLONED_MESH);

    const { operation, joined } = useCloneActivityStore.getState().startClone(TARGET);

    expect(joined).toBe(false);
    expect(operation.status).toBe('running');
    expect(operation.repo).toBe(TARGET.repo);
    expect(operation.destination).toBe(TARGET.destination);
    expect(operation.parentDir).toBe(TARGET.parentDir);
    expect(mockInvoke).toHaveBeenCalledWith('clone_mesh_repo', {
      url: TARGET.repo,
      parentDir: TARGET.parentDir,
      color: TARGET.color,
    });
  });

  it('joins a running clone of the same repository into the same destination', () => {
    pendingClone();

    const first = useCloneActivityStore.getState().startClone(TARGET);
    const second = useCloneActivityStore.getState().startClone(TARGET);

    // The second submission is the SAME operation — no second clone runs.
    expect(second.joined).toBe(true);
    expect(second.operation.id).toBe(first.operation.id);
    expect(useCloneActivityStore.getState().operations).toHaveLength(1);
    expect(
      mockInvoke.mock.calls.filter(([cmd]) => cmd === 'clone_mesh_repo')
    ).toHaveLength(1);
  });

  it('treats a github.com URL as the same repository as its owner/repo form', () => {
    pendingClone();

    const first = useCloneActivityStore
      .getState()
      .startClone({ ...TARGET, repo: 'alondero/buildmesh' });
    const second = useCloneActivityStore
      .getState()
      .startClone({ ...TARGET, repo: 'https://github.com/alondero/buildmesh' });

    expect(second.joined).toBe(true);
    expect(second.operation.id).toBe(first.operation.id);
    expect(
      mockInvoke.mock.calls.filter(([cmd]) => cmd === 'clone_mesh_repo')
    ).toHaveLength(1);
  });

  it('starts a separate operation for a different destination', () => {
    pendingClone();
    mockInvoke.mockReturnValueOnce(new Promise(() => {}));

    const first = useCloneActivityStore.getState().startClone(TARGET);
    const second = useCloneActivityStore
      .getState()
      .startClone({ ...TARGET, parentDir: 'D:\\other', destination: 'D:\\other\\buildmesh' });

    expect(second.joined).toBe(false);
    expect(second.operation.id).not.toBe(first.operation.id);
    expect(useCloneActivityStore.getState().operations).toHaveLength(2);
  });

  it('settles a successful clone with the new mesh', async () => {
    mockInvoke.mockResolvedValueOnce(CLONED_MESH);

    const { operation } = useCloneActivityStore.getState().startClone(TARGET);

    await vi.waitFor(() =>
      expect(useCloneActivityStore.getState().operations[0].status).toBe('succeeded')
    );
    expect(useCloneActivityStore.getState().operations[0].meshId).toBe(CLONED_MESH.id);
    expect(useCloneActivityStore.getState().operations[0].meshName).toBe(CLONED_MESH.name);
    expect(useCloneActivityStore.getState().operations[0].finishedAt).not.toBeNull();
    expect(operation.error).toBeNull();
  });

  it('settles a failed clone with the backend message', async () => {
    mockInvoke.mockRejectedValueOnce(new Error('git clone failed: Repository not found'));

    useCloneActivityStore.getState().startClone(TARGET);

    await vi.waitFor(() =>
      expect(useCloneActivityStore.getState().operations[0].status).toBe('failed')
    );
    expect(useCloneActivityStore.getState().operations[0].error).toMatch(/Repository not found/);
    expect(useCloneActivityStore.getState().operations[0].meshId).toBeNull();
  });

  it('reports the outcome as a toast when nothing is showing it', async () => {
    mockInvoke.mockRejectedValueOnce(new Error('git clone failed: Repository not found'));

    useCloneActivityStore.getState().startClone(TARGET);

    await vi.waitFor(() => expect(useToastStore.getState().toasts).toHaveLength(1));
    expect(useToastStore.getState().toasts[0].severity).toBe('error');
    expect(useToastStore.getState().toasts[0].message).toMatch(/Repository not found/);
  });

  it('does not report the outcome twice when a surface is showing it', async () => {
    mockInvoke.mockRejectedValueOnce(new Error('git clone failed: Repository not found'));

    const { operation } = useCloneActivityStore.getState().startClone(TARGET);
    useCloneActivityStore.getState().claimOperation(operation.id);

    await vi.waitFor(() =>
      expect(useCloneActivityStore.getState().operations[0].status).toBe('failed')
    );
    expect(useToastStore.getState().toasts).toHaveLength(0);

    // The outcome was shown by the surface, so it is not repeated as a
    // toast afterwards.
    useCloneActivityStore.getState().releaseOperation(operation.id);
    expect(useToastStore.getState().toasts).toHaveLength(0);
  });

  // ---- Review round 1: the claim is a count, not a set ------------------------

  it('keeps the claim while another surface still holds it', async () => {
    mockInvoke.mockRejectedValueOnce(new Error('git clone failed: Repository not found'));

    const { operation } = useCloneActivityStore.getState().startClone(TARGET);
    const store = useCloneActivityStore.getState();
    // Two surfaces show the same operation — the create dialog and the
    // progress surface.
    store.claimOperation(operation.id);
    store.claimOperation(operation.id);
    expect(useCloneActivityStore.getState().claimedOperationCounts[operation.id]).toBe(2);

    // The first one closes: the other still holds a claim, so the outcome is
    // NOT reported a second time.
    useCloneActivityStore.getState().releaseOperation(operation.id);
    expect(useCloneActivityStore.getState().claimedOperationCounts[operation.id]).toBe(1);

    await vi.waitFor(() =>
      expect(useCloneActivityStore.getState().operations[0].status).toBe('failed')
    );
    expect(useToastStore.getState().toasts).toHaveLength(0);

    // The last holder lets go: the operation is no longer displayed.
    useCloneActivityStore.getState().releaseOperation(operation.id);
    expect(useCloneActivityStore.getState().claimedOperationCounts[operation.id]).toBeUndefined();
  });

  it('forgets a settled operation but never a running one', () => {
    pendingClone();

    const { operation } = useCloneActivityStore.getState().startClone(TARGET);
    useCloneActivityStore.getState().dismissOperation(operation.id);
    expect(useCloneActivityStore.getState().operations).toHaveLength(1);

    useCloneActivityStore.setState(state => ({
      operations: state.operations.map(op =>
        op.id === operation.id ? { ...op, status: 'failed' as const, error: 'nope' } : op
      ),
    }));
    useCloneActivityStore.getState().dismissOperation(operation.id);
    expect(useCloneActivityStore.getState().operations).toHaveLength(0);
  });

  // ---- Review round 1: every live operation is reachable --------------------

  it('lists every live operation, running ones first', () => {
    pendingClone();
    mockInvoke.mockReturnValueOnce(new Promise(() => {}));

    const first = useCloneActivityStore.getState().startClone(TARGET).operation;
    const second = useCloneActivityStore
      .getState()
      .startClone({ ...TARGET, repo: 'alondero/other', destination: 'D:\\repos\\other' })
      .operation;
    const settled = {
      ...first,
      id: 'clone-settled',
      status: 'failed' as const,
      error: 'nope',
      finishedAt: Date.now(),
    };
    useCloneActivityStore.setState(state => ({
      operations: [settled, second, first],
    }));

    const visible = selectVisibleOperations(useCloneActivityStore.getState().operations);
    // Both running clones are listed — a second clone with no entry of its
    // own would be invisible while it runs.
    expect(visible.map(op => op.id)).toEqual([second.id, first.id, settled.id]);
  });

  it('retries from the submitted target', async () => {
    mockInvoke.mockRejectedValueOnce(new Error('git clone failed: Repository not found'));

    const { operation } = useCloneActivityStore.getState().startClone(TARGET);
    await vi.waitFor(() =>
      expect(useCloneActivityStore.getState().operations[0].status).toBe('failed')
    );

    mockInvoke.mockResolvedValueOnce(CLONED_MESH);
    const retry = useCloneActivityStore.getState().retryClone(operation.id);

    expect(retry).not.toBeNull();
    expect(retry!.operation.id).not.toBe(operation.id);
    // The retry carries the submitted target verbatim — a later edit of the
    // form cannot retarget it.
    expect(retry!.operation.repo).toBe(TARGET.repo);
    expect(retry!.operation.destination).toBe(TARGET.destination);
    expect(mockInvoke).toHaveBeenLastCalledWith('clone_mesh_repo', {
      url: TARGET.repo,
      parentDir: TARGET.parentDir,
      color: TARGET.color,
    });
  });
});
