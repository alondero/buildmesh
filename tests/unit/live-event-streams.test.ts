/**
 * The live event stream owners (issue #2531).
 *
 * Each stream has exactly one owner module under `src/lib/streams/`. The
 * owner holds the ONE backend subscription, converts the raw payload into
 * a typed update, and owns the match rule ("is this update about MY
 * run / MY mesh?"). These tests pin the two halves of that contract:
 *
 *   1. The payload → typed-update conversion, including the "not my run"
 *      case, so a consumer never has to match a raw payload itself.
 *   2. The subscription contract: one backend listener per stream however
 *      many consumers read it, and the listener is released when the last
 *      consumer goes.
 */
import { describe, it, expect, vi } from 'vitest';
import { renderHook, act } from '@testing-library/react';
import { emit, listen } from '@tauri-apps/api/event';
import {
  concernsRun,
  subscribeCircuitRunUpdates,
  toCircuitRunUpdate,
  useCircuitRunUpdates,
  type CircuitRunUpdate,
} from '../../src/lib/streams/circuitRunStream';
import {
  subscribeProviderListChanges,
  toProviderListChange,
  type ProviderListChange,
} from '../../src/lib/streams/providerListStream';
import {
  concernsMesh,
  subscribeWorktreeDirectoryChanges,
  toWorktreeDirectoryChange,
  type WorktreeDirectoryChange,
} from '../../src/lib/streams/worktreeDirectoryStream';
import {
  concernsMeshPath,
  subscribeMeshSyncWarnings,
  toMeshSyncWarning,
  type MeshSyncWarning,
} from '../../src/lib/streams/meshSyncWarningStream';

/** How many times `listen` was called for `event`. */
function registrationsFor(event: string): number {
  return vi.mocked(listen).mock.calls.filter(([name]) => name === event).length;
}

describe('circuitRunStream — payload conversion', () => {
  it('derives the run-state vocabulary once, in the owner', () => {
    const live = toCircuitRunUpdate({ run_id: 7, state: 'running' });
    expect(live).toEqual({
      kind: 'transition',
      runId: 7,
      state: 'running',
      known: true,
      live: true,
      terminal: false,
    });

    const terminal = toCircuitRunUpdate({ run_id: 7, state: 'failed' });
    expect(terminal).toMatchObject({ known: true, live: false, terminal: true });

    // A state the worker never emits is reported as unknown rather than
    // guessed at — the old subscribers each kept their own copy of these
    // sets and could drift apart.
    const unknown = toCircuitRunUpdate({ run_id: 7, state: 'zombie' });
    expect(unknown).toMatchObject({ known: false, live: false, terminal: false });
  });

  it('reports a payload that names no run as unattributed', () => {
    // The worker always sends `{ run_id, state }`, but the event firing at
    // all still means the circuit subsystem moved — the surfaces that
    // re-read the whole snapshot depend on that.
    expect(toCircuitRunUpdate({})).toEqual({ kind: 'unattributed' });
    expect(toCircuitRunUpdate(null)).toEqual({ kind: 'unattributed' });
    expect(toCircuitRunUpdate('nope')).toEqual({ kind: 'unattributed' });
    expect(toCircuitRunUpdate({ run_id: 'seven', state: 'running' })).toEqual({ kind: 'unattributed' });
    expect(toCircuitRunUpdate({ run_id: 7 })).toEqual({ kind: 'unattributed' });
  });

  it('answers "is this about my run?" in one place', () => {
    const mine = toCircuitRunUpdate({ run_id: 7, state: 'running' });
    expect(concernsRun(mine, 7)).toBe(true);
    // The "not my run" case: another run's transition is not this
    // consumer's business.
    expect(concernsRun(mine, 8)).toBe(false);
    // An unattributed update belongs to no run.
    expect(concernsRun(toCircuitRunUpdate({}), 7)).toBe(false);
  });
});

describe('circuitRunStream — subscription contract', () => {
  it('holds one backend listener however many consumers read it', async () => {
    const seen: CircuitRunUpdate[] = [];
    const releaseFirst = subscribeCircuitRunUpdates((update) => seen.push(update));
    const releaseSecond = subscribeCircuitRunUpdates((update) => seen.push(update));

    expect(registrationsFor('circuit-run-updated')).toBe(1);

    await emit('circuit-run-updated', { run_id: 7, state: 'running' });

    expect(seen).toHaveLength(2);
    expect(seen[0]).toMatchObject({ kind: 'transition', runId: 7, state: 'running' });

    releaseFirst();
    releaseSecond();
  });

  it('releases the backend listener when the last consumer goes', async () => {
    const seen: CircuitRunUpdate[] = [];
    const release = subscribeCircuitRunUpdates((update) => seen.push(update));
    await emit('circuit-run-updated', { run_id: 7, state: 'running' });
    expect(seen).toHaveLength(1);

    release();
    await emit('circuit-run-updated', { run_id: 7, state: 'running' });

    // No consumer left, so no update is published and the backend
    // listener is gone.
    expect(seen).toHaveLength(1);
  });

  it('publishes only the transitions of the run in scope', async () => {
    const { result } = renderHook(() => useCircuitRunUpdates(7));

    await act(async () => {
      await emit('circuit-run-updated', { run_id: 8, state: 'running' });
    });
    // Another run's transition is not this consumer's business.
    expect(result.current).toHaveLength(0);

    await act(async () => {
      await emit('circuit-run-updated', { run_id: 7, state: 'running' });
    });
    expect(result.current).toHaveLength(1);
    expect(result.current[0]).toMatchObject({ kind: 'transition', runId: 7 });
  });

  it('subscribes to nothing when no run is in scope', async () => {
    const { result } = renderHook(() => useCircuitRunUpdates(null));
    await act(async () => {
      await emit('circuit-run-updated', { run_id: 7, state: 'running' });
    });
    expect(result.current).toHaveLength(0);
  });

  it('changes its value once per run switch, so a consumer refetches once', async () => {
    let renders = 0;
    const { rerender } = renderHook(
      ({ runId }: { runId: number }) => {
        renders += 1;
        return useCircuitRunUpdates(runId);
      },
      { initialProps: { runId: 7 } },
    );

    await act(async () => {
      await emit('circuit-run-updated', { run_id: 7, state: 'running' });
    });

    const before = renders;
    rerender({ runId: 8 });

    // The previous run's transitions do not belong to run 8, so the value
    // changes exactly once. Clearing them from an effect instead would
    // change it a second time, and a consumer whose effect depends on it
    // would fetch the new run twice.
    expect(renders - before).toBe(1);
  });
});

describe('providerListStream — payload conversion', () => {
  it('accepts the unit payload the backend and the frontend both emit', () => {
    // The backend emits `()`; the frontend's own `emit(name)` sends
    // `undefined`. Both mean "the catalogue changed".
    expect(toProviderListChange(null)).toEqual({ kind: 'provider-list-changed' });
    expect(toProviderListChange(undefined)).toEqual({ kind: 'provider-list-changed' });
  });

  it('drops a payload this stream cannot describe', () => {
    expect(toProviderListChange({ run_id: 1 })).toBeNull();
    expect(toProviderListChange('provider-list-changed')).toBeNull();
  });
});

describe('providerListStream — subscription contract', () => {
  it('holds one backend listener however many consumers read it', async () => {
    const seen: ProviderListChange[] = [];
    const releaseFirst = subscribeProviderListChanges((change) => seen.push(change));
    const releaseSecond = subscribeProviderListChanges((change) => seen.push(change));

    expect(registrationsFor('provider-list-changed')).toBe(1);

    await emit('provider-list-changed', undefined);

    expect(seen).toHaveLength(2);
    expect(seen[0]).toEqual({ kind: 'provider-list-changed' });

    releaseFirst();
    releaseSecond();
  });
});

describe('worktreeDirectoryStream — payload conversion', () => {
  it('names the mesh whose directory moved', () => {
    expect(toWorktreeDirectoryChange(7)).toEqual({ meshId: 7 });
  });

  it('reads a null payload as "the app-wide default moved"', () => {
    expect(toWorktreeDirectoryChange(null)).toEqual({ meshId: null });
    expect(toWorktreeDirectoryChange(undefined)).toEqual({ meshId: null });
  });

  it('drops a payload this stream cannot describe', () => {
    expect(toWorktreeDirectoryChange('seven')).toBeNull();
    expect(toWorktreeDirectoryChange({ mesh_id: 7 })).toBeNull();
  });

  it('answers "is this about my mesh?" in one place', () => {
    const mine = toWorktreeDirectoryChange(7)!;
    expect(concernsMesh(mine, 7)).toBe(true);
    // The "not my mesh" case: another mesh's directory moved.
    expect(concernsMesh(mine, 8)).toBe(false);
    // The app-wide default moved, so every mesh has to re-resolve.
    expect(concernsMesh(toWorktreeDirectoryChange(null)!, 8)).toBe(true);
  });
});

describe('worktreeDirectoryStream — subscription contract', () => {
  it('holds one backend listener however many consumers read it', async () => {
    const seen: WorktreeDirectoryChange[] = [];
    const releaseFirst = subscribeWorktreeDirectoryChanges((change) => seen.push(change));
    const releaseSecond = subscribeWorktreeDirectoryChanges((change) => seen.push(change));

    expect(registrationsFor('worktree-directory-changed')).toBe(1);

    await emit('worktree-directory-changed', 7);

    expect(seen).toHaveLength(2);
    expect(seen[0]).toEqual({ meshId: 7 });

    releaseFirst();
    releaseSecond();
  });
});

describe('meshSyncWarningStream — payload conversion', () => {
  it('keeps the toast message and the mesh the warning is about', () => {
    const warning = toMeshSyncWarning({ message: 'Could not reach origin', mesh_path: '/repo/mesh' });
    expect(warning).toEqual({ message: 'Could not reach origin', meshPath: '/repo/mesh' });
  });

  it('drops a payload without a message', () => {
    expect(toMeshSyncWarning({ mesh_path: '/repo/mesh' })).toBeNull();
    expect(toMeshSyncWarning(null)).toBeNull();
    expect(toMeshSyncWarning('boom')).toBeNull();
  });

  it('answers "is this about my mesh?" in one place', () => {
    const warning = toMeshSyncWarning({ message: 'boom', mesh_path: '/repo/mesh' })!;
    expect(concernsMeshPath(warning, '/repo/mesh')).toBe(true);
    // The "not my mesh" case: the warning names a different mesh.
    expect(concernsMeshPath(warning, '/repo/other')).toBe(false);
    // A warning that named no mesh belongs to no mesh.
    const unnamed = toMeshSyncWarning({ message: 'boom' })!;
    expect(concernsMeshPath(unnamed, '/repo/mesh')).toBe(false);
  });
});

describe('meshSyncWarningStream — subscription contract', () => {
  it('holds one backend listener however many consumers read it', async () => {
    const seen: MeshSyncWarning[] = [];
    const releaseFirst = subscribeMeshSyncWarnings((warning) => seen.push(warning));
    const releaseSecond = subscribeMeshSyncWarnings((warning) => seen.push(warning));

    expect(registrationsFor('mesh-sync-warning')).toBe(1);

    await emit('mesh-sync-warning', { message: 'boom', mesh_path: '/repo/mesh' });

    expect(seen).toHaveLength(2);
    expect(seen[0]).toEqual({ message: 'boom', meshPath: '/repo/mesh' });

    releaseFirst();
    releaseSecond();
  });
});
