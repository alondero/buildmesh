import { describe, it, expect, beforeEach, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';

// Mock frontendLog so the wrapper's log call is observable without going
// through the (already-tested) bridge. The wrapper imports the real module
// in production; this mock replaces it for the test.
vi.mock('../../src/lib/frontendLog', () => ({
  logFrontend: vi.fn(),
  installFrontendLogBridge: vi.fn(),
}));

// Import AFTER the mock so the wrapper picks up the mocked logFrontend.
import * as api from '../../src/lib/tauri';
import { __resetProviderCachesForTests } from '../../src/lib/tauri';

const mockInvoke = vi.mocked(invoke);

function mockProviderIpc() {
  mockInvoke.mockImplementation((cmd: string) => {
    if (cmd === 'get_default_provider') return Promise.resolve('minimax');
    if (cmd === 'list_providers') {
      return Promise.resolve([
        { id: 'anthropic', label: 'Claude' },
        { id: 'minimax', label: 'MiniMax' },
      ]);
    }
    return Promise.resolve(undefined);
  });
}

function callsTo(cmd: string): number {
  return mockInvoke.mock.calls.filter(c => c[0] === cmd).length;
}

describe('tauri.ts provider memoisation (#405)', () => {
  beforeEach(() => {
    mockInvoke.mockReset();
    __resetProviderCachesForTests();
  });

  it('listProviders() returns the catalogue shape from the wrapper', async () => {
    mockProviderIpc();
    const providers = await api.listProviders();
    expect(providers).toEqual([
      { id: 'anthropic', label: 'Claude' },
      { id: 'minimax', label: 'MiniMax' },
    ]);
  });

  it('getDefaultProvider(meshId) returns the default id from the wrapper', async () => {
    mockProviderIpc();
    expect(await api.getDefaultProvider(1)).toBe('minimax');
  });

  it('shares one list_providers call and one get_default_provider call across N nodes of a mesh', async () => {
    mockProviderIpc();

    // Simulate N panes of the same mesh all resolving their label on mount.
    await Promise.all([
      Promise.all([api.listProviders(), api.getDefaultProvider(1)]),
      Promise.all([api.listProviders(), api.getDefaultProvider(1)]),
      Promise.all([api.listProviders(), api.getDefaultProvider(1)]),
      Promise.all([api.listProviders(), api.getDefaultProvider(1)]),
    ]);

    expect(callsTo('get_default_provider')).toBe(1);
    expect(callsTo('list_providers')).toBe(1);
  });

  it('fetches the default provider once per mesh but reuses the global provider list', async () => {
    mockProviderIpc();

    await api.getDefaultProvider(1);
    await api.getDefaultProvider(2);
    await api.listProviders();
    await api.listProviders();

    expect(callsTo('get_default_provider')).toBe(2);
    expect(callsTo('list_providers')).toBe(1);
  });

  it('two concurrent api.listProviders() calls share one IPC round-trip', async () => {
    // Make the IPC intentionally take a few microtasks so both calls land
    // before the first one resolves — this is the dedup window the cache
    // exists for. A synchronous resolve would still pass (the if-check on
    // the cached promise is the load-bearing bit), but the deferred
    // version exercises the in-flight promise path.
    let resolveList!: (v: unknown) => void;
    mockInvoke.mockImplementationOnce((cmd: string) => {
      if (cmd === 'list_providers') return new Promise(r => { resolveList = r; });
      return Promise.resolve(undefined);
    });

    const a = api.listProviders();
    const b = api.listProviders();
    // Both calls attached to the same in-flight promise — neither has
    // resolved yet, so the cache must have de-duped them onto one IPC.
    expect(callsTo('list_providers')).toBe(1);

    resolveList([{ id: 'anthropic', label: 'Claude' }]);
    const [ra, rb] = await Promise.all([a, b]);
    expect(ra).toBe(rb); // strictly the same resolved value
    expect(callsTo('list_providers')).toBe(1);
  });

  it('a rejected api.listProviders() is evicted and the next call retries', async () => {
    mockInvoke.mockRejectedValueOnce(new Error('boom'));
    await expect(api.listProviders()).rejects.toThrow('boom');

    // The second call must NOT inherit the rejected promise — it has to
    // re-hit IPC. Re-mock for success, then assert it resolves.
    mockProviderIpc();
    const result = await api.listProviders();
    expect(result).toEqual([
      { id: 'anthropic', label: 'Claude' },
      { id: 'minimax', label: 'MiniMax' },
    ]);
    expect(callsTo('list_providers')).toBe(2);
  });

  it('a rejected api.getDefaultProvider(meshId) is evicted for that mesh and the next call retries', async () => {
    mockInvoke.mockImplementation((cmd: string) => {
      if (cmd === 'get_default_provider') return Promise.reject(new Error('no mesh'));
      return Promise.resolve(undefined);
    });
    await expect(api.getDefaultProvider(9)).rejects.toThrow('no mesh');

    // The mesh-9 entry should be evicted, mesh-1 untouched.
    mockProviderIpc();
    expect(await api.getDefaultProvider(9)).toBe('minimax');
    expect(callsTo('get_default_provider')).toBe(2);
  });

  // Issue #534: adding/removing a provider account can register or drop a
  // paired harness profile, which changes the spawn-menu catalogue. The cached
  // provider list must be busted so the next read re-hits IPC instead of
  // serving the pre-change list (the reported "Kimi never shows up" bug).
  it('upsertProviderAccount() invalidates the cached provider list', async () => {
    mockProviderIpc();
    await api.listProviders();
    expect(callsTo('list_providers')).toBe(1);

    await api.upsertProviderAccount({
      id: 'kimi',
      name: 'Kimi',
      enabled: true,
      billing_mode: 'pay_as_you_go',
      claude_compatible: true,
      api_key: 'k',
      base_url: 'https://api.moonshot.cn/anthropic',
      model_tiers: { default: null, small_fast: null, sonnet: null, opus: null, haiku: null },
      models: [],
    });

    await api.listProviders();
    expect(callsTo('list_providers')).toBe(2);
  });

  it('removeProviderAccount() invalidates the cached provider list', async () => {
    mockProviderIpc();
    await api.listProviders();
    expect(callsTo('list_providers')).toBe(1);

    await api.removeProviderAccount('kimi');

    await api.listProviders();
    expect(callsTo('list_providers')).toBe(2);
  });
});
