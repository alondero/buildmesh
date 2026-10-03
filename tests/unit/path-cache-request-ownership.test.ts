import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { emit } from '@tauri-apps/api/event';
import { act, renderHook } from '@testing-library/react';
import { usePathInvalidatedQuery } from '../../src/hooks/usePathInvalidatedQuery';
import {
  createDualKeyCache,
  createPathKeyedCache,
  resetPathInvalidatedCacheForTests,
} from '../../src/lib/pathInvalidatedCache';

function deferred() {
  let resolve!: (value: string | null) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<string | null>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

beforeEach(() => resetPathInvalidatedCacheForTests());
afterEach(() => {
  resetPathInvalidatedCacheForTests();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

describe('Git cache request ownership', () => {
  it('lets a slow request finish during continuous edits and coalesces one trailing fetch', async () => {
    vi.useFakeTimers();
    const first = deferred();
    const settled = deferred();
    const fetcher = vi.fn().mockReturnValueOnce(first.promise).mockReturnValueOnce(settled.promise);
    const client = createPathKeyedCache<string>({ fetcher, minRefetchIntervalMs: 60_000 });
    const refreshes: Promise<string | null>[] = [];
    client.subscribe('/repo', () => { refreshes.push(client.refresh('/repo')); });
    await emit('git-changed', { path: '/repo' });
    await vi.advanceTimersByTimeAsync(500);
    await emit('git-changed', { path: '/repo' });
    expect(fetcher).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(100);
    first.resolve('live snapshot');
    expect(await refreshes[0]).toBe('live snapshot');
    expect(client.read('/repo')).toBe('live snapshot');
    for (let i = 0; i < 10; i++) {
      await vi.advanceTimersByTimeAsync(500);
      await emit('git-changed', { path: '/repo' });
    }
    expect(fetcher).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(1);
    await vi.advanceTimersByTimeAsync(55_000);
    expect(fetcher).toHaveBeenCalledTimes(2);
    settled.resolve('settled snapshot');
    expect(await refreshes[1]).toBe('settled snapshot');
    expect(client.read('/repo')).toBe('settled snapshot');
    expect(vi.getTimerCount()).toBe(0);
  });

  it('publishes data and clears the hook loading state while edits continue', async () => {
    vi.useFakeTimers();
    const first = deferred();
    const settled = deferred();
    const fetcher = vi.fn().mockReturnValueOnce(first.promise).mockReturnValueOnce(settled.promise);
    const client = createPathKeyedCache<string>({ fetcher, minRefetchIntervalMs: 2000 });
    const { result, unmount } = renderHook(() => usePathInvalidatedQuery(client, '/repo'));
    expect(result.current.loading).toBe(true);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
      await emit('git-changed', { path: '/repo' });
      first.resolve('live snapshot');
    });
    expect(result.current.data).toBe('live snapshot');
    expect(result.current.loading).toBe(false);
    await act(async () => {
      await vi.advanceTimersByTimeAsync(2000);
    });
    expect(result.current.loading).toBe(true);
    await act(async () => { settled.resolve('settled snapshot'); });
    expect(result.current.data).toBe('settled snapshot');
    expect(result.current.loading).toBe(false);
    expect(fetcher).toHaveBeenCalledTimes(2);
    unmount();
  });

  it('coalesces edits during a slow request even with no freshness window', async () => {
    vi.useFakeTimers();
    const first = deferred();
    const second = deferred();
    const third = deferred();
    const fetcher = vi.fn().mockReturnValueOnce(first.promise).mockReturnValueOnce(second.promise).mockReturnValueOnce(third.promise);
    const client = createPathKeyedCache<string>({ fetcher });
    const refreshes: Promise<string | null>[] = [];
    client.subscribe('/repo', () => { refreshes.push(client.refresh('/repo')); });
    await emit('git-changed', { path: '/repo' });
    await emit('git-changed', { path: '/repo' });
    first.resolve('first');
    expect(await refreshes[0]).toBe('first');
    await vi.advanceTimersByTimeAsync(0);
    for (let i = 0; i < 20; i++) await emit('git-changed', { path: '/repo' });
    expect(fetcher).toHaveBeenCalledTimes(2);
    second.resolve('second');
    expect(await refreshes[1]).toBe('second');
    await vi.advanceTimersByTimeAsync(0);
    expect(fetcher).toHaveBeenCalledTimes(3);
    third.resolve('settled');
    expect(await refreshes[2]).toBe('settled');
    expect(client.read('/repo')).toBe('settled');
    expect(vi.getTimerCount()).toBe(0);
  });

  it('retains a dirty refresh when the running request fails', async () => {
    vi.useFakeTimers();
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    const first = deferred();
    const retry = deferred();
    const fetcher = vi.fn().mockReturnValueOnce(first.promise).mockReturnValueOnce(retry.promise);
    const client = createPathKeyedCache<string>({ fetcher, minRefetchIntervalMs: 2000 });
    const refreshes: Promise<string | null>[] = [];
    client.subscribe('/repo', () => { refreshes.push(client.refresh('/repo')); });
    await emit('git-changed', { path: '/repo' });
    await emit('git-changed', { path: '/repo' });
    expect(fetcher).toHaveBeenCalledTimes(1);
    const error = new Error('first fetch failed');
    first.reject(error);
    expect(await refreshes[0]).toBeNull();
    expect(client.lastError('/repo')).toBe(error);
    await vi.advanceTimersByTimeAsync(0);
    expect(fetcher).toHaveBeenCalledTimes(2);
    retry.resolve('recovered');
    expect(await refreshes[1]).toBe('recovered');
    expect(client.lastError('/repo')).toBeNull();
    expect(client.read('/repo')).toBe('recovered');
  });

  it.each(['event', 'manual'] as const)('shares one request across 20 subscribers on %s invalidation', async (source) => {
    const request = deferred();
    const fetcher = vi.fn(() => request.promise);
    const client = createPathKeyedCache({ fetcher });
    const refreshes: Promise<string | null>[] = [];
    const callbacks = Array.from({ length: 20 }, () => vi.fn(() => {
      refreshes.push(client.refresh('/repo'));
    }));
    callbacks.forEach(callback => client.subscribe('/repo', callback));

    if (source === 'event') await emit('git-changed', { path: '/repo' });
    else client.notifyByPath('/repo');

    callbacks.forEach(callback => expect(callback).toHaveBeenCalledTimes(1));
    expect(fetcher).toHaveBeenCalledTimes(1);
    expect(new Set(refreshes).size).toBe(1);
    request.resolve('current');
    expect(await Promise.all(refreshes)).toEqual(Array(20).fill('current'));
  });

  it('deduplicates the same dual key across matching paths while preserving other keys and clients', async () => {
    const fetcher = vi.fn().mockResolvedValue('value');
    const client = createDualKeyCache<number, string>({ fetcher });
    const siblingFetcher = vi.fn().mockResolvedValue('sibling');
    const sibling = createDualKeyCache<number, string>({ fetcher: siblingFetcher });
    const parent = vi.fn(() => { void client.refresh(7); });
    const child = vi.fn(() => { void client.refresh(7); });
    client.subscribeByPath(7, '/repo', parent);
    client.subscribeByPath(7, '/alias', child, ['/repo']);
    client.subscribeByPath(8, '/repo', () => { void client.refresh(8); });
    sibling.subscribeByPath(7, '/repo', () => { void sibling.refresh(7); });

    await emit('git-changed', { path: '/repo' });

    expect(parent).toHaveBeenCalledTimes(1);
    expect(child).toHaveBeenCalledTimes(1);
    expect(fetcher.mock.calls).toEqual([[7], [8]]);
    expect(siblingFetcher.mock.calls).toEqual([[7]]);
  });

  it.each(['old-first', 'new-first'] as const)('keeps the newest request authoritative when results resolve %s', async (order) => {
    const old = deferred();
    const current = deferred();
    const fetcher = vi.fn().mockReturnValueOnce(old.promise).mockReturnValueOnce(current.promise);
    const client = createPathKeyedCache<string>({ fetcher });
    const oldRefresh = client.refresh('/repo');
    client.invalidate('/repo');
    const currentRefresh = client.refresh('/repo');

    if (order === 'old-first') {
      old.resolve('obsolete');
      await Promise.resolve();
      expect(client.read('/repo')).toBeUndefined();
      expect(client.refresh('/repo')).toBe(currentRefresh);
      expect(fetcher).toHaveBeenCalledTimes(2);
      current.resolve('current');
    } else {
      current.resolve('current');
      await currentRefresh;
      old.resolve('obsolete');
    }

    expect(await currentRefresh).toBe('current');
    expect(await oldRefresh).toBe('current');
    expect(client.read('/repo')).toBe('current');
    expect(client.lastError('/repo')).toBeNull();
  });

  it.each(['old-first', 'new-first'] as const)('ignores an obsolete rejection when it arrives %s', async (order) => {
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    const old = deferred();
    const current = deferred();
    const client = createPathKeyedCache<string>({
      fetcher: vi.fn().mockReturnValueOnce(old.promise).mockReturnValueOnce(current.promise),
    });
    const oldRefresh = client.refresh('/repo');
    client.invalidate('/repo');
    const currentRefresh = client.refresh('/repo');
    if (order === 'new-first') {
      current.resolve('current');
      await currentRefresh;
    }
    old.reject(new Error('obsolete error'));
    await Promise.resolve();
    await Promise.resolve();
    if (order === 'old-first') {
      expect(client.refresh('/repo')).toBe(currentRefresh);
      expect(client.lastError('/repo')).toBeNull();
      current.resolve('current');
    }
    expect(await oldRefresh).toBe('current');
    expect(await currentRefresh).toBe('current');
    expect(client.lastError('/repo')).toBeNull();
  });

  it('does not repopulate an invalidated key when there is no replacement request', async () => {
    const request = deferred();
    const client = createPathKeyedCache({ fetcher: () => request.promise });
    const refresh = client.refresh('/repo');
    client.invalidate('/repo');
    request.resolve('obsolete');
    expect(await refresh).toBeNull();
    expect(client.read('/repo')).toBeUndefined();
  });

  it('preserves the current error when an obsolete success arrives', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    const old = deferred();
    const current = deferred();
    const client = createPathKeyedCache<string>({
      fetcher: vi.fn().mockReturnValueOnce(old.promise).mockReturnValueOnce(current.promise),
    });
    const oldRefresh = client.refresh('/repo');
    client.invalidate('/repo');
    const currentRefresh = client.refresh('/repo');
    const error = new Error('current error');
    current.reject(error);
    expect(await currentRefresh).toBeNull();
    old.resolve('obsolete');
    expect(await oldRefresh).toBeNull();
    expect(client.lastError('/repo')).toBe(error);
    expect(client.read('/repo')).toBeUndefined();
  });
});

describe('Git cache trailing subscription cleanup', () => {
  it('skips a sibling unsubscribed synchronously by an earlier trailing callback', async () => {
    vi.useFakeTimers();
    const client = createPathKeyedCache({ fetcher: vi.fn().mockResolvedValue('value'), minRefetchIntervalMs: 2000 });
    await client.refresh('/repo');
    let unsubscribeSecond = () => {};
    const first = vi.fn(() => unsubscribeSecond());
    const second = vi.fn();
    client.subscribe('/repo', first);
    unsubscribeSecond = client.subscribe('/repo', second);
    await emit('git-changed', { path: '/repo' });
    await vi.advanceTimersByTimeAsync(2000);
    expect(first).toHaveBeenCalledTimes(1);
    expect(second).not.toHaveBeenCalled();
  });

  it('releases the timer and callback after the last subscriber unmounts', async () => {
    vi.useFakeTimers();
    const client = createPathKeyedCache({ fetcher: vi.fn().mockResolvedValue('value'), minRefetchIntervalMs: 2000 });
    await client.refresh('/repo');
    const callback = vi.fn();
    const unsubscribe = client.subscribe('/repo', callback);
    await emit('git-changed', { path: '/repo' });
    expect(vi.getTimerCount()).toBe(1);
    unsubscribe();
    unsubscribe();
    expect(vi.getTimerCount()).toBe(0);
    await vi.advanceTimersByTimeAsync(2000);
    expect(callback).not.toHaveBeenCalled();
    expect(client.read('/repo')).toBe('value');
  });

  it('keeps a sibling subscription with the same callback alive and shares its trailing request', async () => {
    vi.useFakeTimers();
    const fetcher = vi.fn().mockResolvedValue('value');
    const client = createPathKeyedCache({ fetcher, minRefetchIntervalMs: 2000 });
    await client.refresh('/repo');
    const callback = vi.fn(() => { void client.refresh('/repo'); });
    const unsubscribe = client.subscribe('/repo', callback);
    client.subscribe('/repo', callback);
    client.subscribe('/repo', callback);
    await emit('git-changed', { path: '/repo' });
    unsubscribe();
    await vi.advanceTimersByTimeAsync(2000);
    expect(callback).toHaveBeenCalledTimes(2);
    expect(fetcher).toHaveBeenCalledTimes(2);
  });
});
