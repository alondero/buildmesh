/**
 * Per-entity promise-map retention audit (issue #2017).
 *
 * Two caches outside the path-invalidated primitive are keyed by Mesh id
 * and have no size bound, so they cannot be reclaimed by its retention
 * policy and must be evicted at the lifecycle boundary instead:
 *
 *   - `providerCache.defaultProviderByMesh` — resolved default provider
 *   - `tauri.scratchpadByMesh` / `scratchpadWritesByMesh` — mesh notes
 *
 * Each case drives the real exported wrappers and observes eviction
 * through the IPC call count, so it fails if a slot is retained even when
 * the internals happen to be renamed.
 */

import { describe, it, expect, beforeEach, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';

vi.mock('../../src/lib/frontendLog', () => ({
  logFrontend: vi.fn(),
  installFrontendLogBridge: vi.fn(),
}));

import * as api from '../../src/lib/tauri';
import { __resetProviderCachesForTests } from '../../src/lib/tauri';

const mockInvoke = vi.mocked(invoke);

function mockMeshIpc() {
  mockInvoke.mockImplementation((cmd: string) => {
    if (cmd === 'get_default_provider') return Promise.resolve('minimax');
    if (cmd === 'get_mesh_scratchpad') return Promise.resolve('# notes');
    if (cmd === 'delete_mesh') return Promise.resolve(undefined);
    return Promise.resolve(undefined);
  });
}

function callsTo(cmd: string): number {
  return mockInvoke.mock.calls.filter((c) => c[0] === cmd).length;
}

describe('per-entity promise retention on Mesh deletion (issue #2017)', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
    __resetProviderCachesForTests();
  });

  it('evicts the deleted Mesh cached default provider', async () => {
    mockMeshIpc();
    expect(await api.getDefaultProvider(1)).toBe('minimax');
    expect(callsTo('get_default_provider')).toBe(1);

    await api.deleteMesh(1);

    // The slot must be gone: the next read re-hits IPC rather than
    // resolving from a promise retained for a Mesh that no longer exists.
    await api.getDefaultProvider(1);
    expect(callsTo('get_default_provider')).toBe(2);
  });

  it('evicts the deleted Mesh cached scratchpad', async () => {
    mockMeshIpc();
    expect(await api.getMeshScratchpad(2)).toBe('# notes');
    expect(callsTo('get_mesh_scratchpad')).toBe(1);

    await api.deleteMesh(2);

    await api.getMeshScratchpad(2);
    expect(callsTo('get_mesh_scratchpad')).toBe(2);
  });

  it('leaves other Meshes cached promises untouched', async () => {
    mockMeshIpc();
    await api.getDefaultProvider(10);
    await api.getDefaultProvider(11);
    await api.getMeshScratchpad(10);
    await api.getMeshScratchpad(11);
    expect(callsTo('get_default_provider')).toBe(2);
    expect(callsTo('get_mesh_scratchpad')).toBe(2);

    await api.deleteMesh(10);

    // Mesh 11 keeps both caches; Mesh 10 re-reads from IPC.
    await api.getDefaultProvider(11);
    await api.getMeshScratchpad(11);
    expect(callsTo('get_default_provider')).toBe(2);
    expect(callsTo('get_mesh_scratchpad')).toBe(2);

    await api.getDefaultProvider(10);
    await api.getMeshScratchpad(10);
    expect(callsTo('get_default_provider')).toBe(3);
    expect(callsTo('get_mesh_scratchpad')).toBe(3);
  });

  it('keeps the cached promises when the delete itself fails', async () => {
    mockMeshIpc();
    await api.getDefaultProvider(3);
    await api.getMeshScratchpad(3);
    expect(callsTo('get_default_provider')).toBe(1);
    expect(callsTo('get_mesh_scratchpad')).toBe(1);

    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'delete_mesh') return Promise.reject(new Error('mesh in use'));
      return Promise.resolve('minimax');
    });
    await expect(api.deleteMesh(3)).rejects.toThrow('mesh in use');

    // The Mesh survived, so its cached promises are still valid — a
    // failure must not evict state that is still authoritative.
    mockMeshIpc();
    await api.getDefaultProvider(3);
    await api.getMeshScratchpad(3);
    expect(callsTo('get_default_provider')).toBe(1);
    expect(callsTo('get_mesh_scratchpad')).toBe(1);
  });

  it('drops the scratchpad write slot so a later write does not queue behind a dead Mesh', async () => {
    let resolveWrite!: () => void;
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'set_mesh_scratchpad') {
        // A write that never settles — the slot will hold this promise.
        return new Promise<void>((resolve) => { resolveWrite = resolve; });
      }
      return Promise.resolve(undefined);
    });

    void api.setMeshScratchpad(4, 'first');
    await Promise.resolve();
    expect(callsTo('set_mesh_scratchpad')).toBe(1);

    // The Mesh is deleted while that write is still in flight.
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'set_mesh_scratchpad') return Promise.resolve(undefined);
      return Promise.resolve(undefined);
    });
    await api.deleteMesh(4);

    const second = api.setMeshScratchpad(4, 'second');
    await Promise.resolve();

    // The second write must reach IPC straight away. Retaining the write
    // slot would chain it behind the abandoned promise, leaving the count
    // at 1 until that promise settled.
    expect(callsTo('set_mesh_scratchpad')).toBe(2);

    resolveWrite();
    await second;
  });
});