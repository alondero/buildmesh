import { describe, it, expect, beforeEach, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import {
  getHandoverProviderLabel,
  __resetHandoverProviderCacheForTests,
} from '../../src/components/Terminal/handoverProviderCache';

const mockInvoke = invoke as ReturnType<typeof vi.fn>;

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

describe('handoverProviderCache', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    __resetHandoverProviderCacheForTests();
  });

  it('resolves the default provider to its display label', async () => {
    mockProviderIpc();
    expect(await getHandoverProviderLabel(1)).toBe('MiniMax');
  });

  it('shares one list_providers call and one get_default_provider call across N nodes of a mesh', async () => {
    mockProviderIpc();

    // Simulate N panes of the same mesh all resolving their label on mount.
    await Promise.all([
      getHandoverProviderLabel(1),
      getHandoverProviderLabel(1),
      getHandoverProviderLabel(1),
      getHandoverProviderLabel(1),
    ]);

    expect(callsTo('get_default_provider')).toBe(1);
    expect(callsTo('list_providers')).toBe(1);
  });

  it('fetches the default provider once per mesh but reuses the global provider list', async () => {
    mockProviderIpc();

    await getHandoverProviderLabel(1);
    await getHandoverProviderLabel(2);

    expect(callsTo('get_default_provider')).toBe(2);
    expect(callsTo('list_providers')).toBe(1);
  });

  it('falls back to "Default" and does not cache a failed lookup', async () => {
    mockInvoke.mockRejectedValueOnce(new Error('no mesh'));
    expect(await getHandoverProviderLabel(9)).toBe('Default');

    // A later call retries rather than inheriting the rejected promise.
    mockProviderIpc();
    expect(await getHandoverProviderLabel(9)).toBe('MiniMax');
  });
});
