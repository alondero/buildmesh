import { describe, it, expect, vi } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import { emit } from '@tauri-apps/api/event';
import {
  createPathKeyedCache,
  createDualKeyCache,
} from '../../src/lib/pathInvalidatedCache';
import { usePathInvalidatedQuery } from '../../src/hooks/usePathInvalidatedQuery';

// Force the helper to behave as on Windows so the slash-normalization and
// case-insensitive branches run, regardless of the host running the suite.
vi.mock('../../src/lib/platform', () => ({
  isMac: false,
  isWindows: true,
}));

// The global vitest setup at tests/setup/vitest.setup.ts resets the
// pathInvalidatedCache primitive (including its `errors` Map) and the
// Tauri event mock listeners between tests, so per-test setup here
// isn't needed.

describe('usePathInvalidatedQuery — error field (issue #342)', () => {
  it('starts with error=null on mount', async () => {
    const client = createPathKeyedCache<string>({
      fetcher: vi.fn().mockResolvedValue('ok'),
    });
    const { result } = renderHook(() =>
      usePathInvalidatedQuery(client, 'k'),
    );
    // Before the fetch resolves, error is still null (initial state).
    expect(result.current.error).toBeNull();
    await waitFor(() => expect(result.current.data).toBe('ok'));
    expect(result.current.error).toBeNull();
  });

  it('populates error with the rejected Error when the mount fetch fails', async () => {
    const boom = new Error('boom');
    const client = createPathKeyedCache<string>({
      fetcher: vi.fn().mockRejectedValue(boom),
    });
    const { result } = renderHook(() =>
      usePathInvalidatedQuery(client, 'k'),
    );
    await waitFor(() => expect(result.current.error).toBe(boom));
    // data is still null because the refresh contract collapses rejection → null.
    expect(result.current.data).toBeNull();
    expect(result.current.loading).toBe(false);
  });

  it('clears error on a subsequent successful fetch', async () => {
    const fetcher = vi
      .fn()
      .mockRejectedValueOnce(new Error('boom'))
      .mockResolvedValueOnce('ok');
    const client = createPathKeyedCache<string>({ fetcher });
    const { result } = renderHook(() =>
      usePathInvalidatedQuery(client, 'k'),
    );
    await waitFor(() => expect(result.current.error).toBeInstanceOf(Error));
    // Trigger a second fetch via refresh().
    await act(async () => {
      result.current.refresh();
    });
    await waitFor(() => expect(result.current.data).toBe('ok'));
    expect(result.current.error).toBeNull();
  });

  it('populates error on a refresh() rejection', async () => {
    const fetcher = vi
      .fn()
      .mockResolvedValueOnce('ok')
      .mockRejectedValueOnce(new Error('refresh-fail'));
    const client = createPathKeyedCache<string>({ fetcher });
    const { result } = renderHook(() =>
      usePathInvalidatedQuery(client, 'k'),
    );
    await waitFor(() => expect(result.current.data).toBe('ok'));
    expect(result.current.error).toBeNull();

    await act(async () => {
      result.current.refresh();
    });
    await waitFor(() => expect(result.current.error).toBeInstanceOf(Error));
    // The second fetcher rejection is captured, data stays as the previous
    // success because `invalidate` wiped the cache and refresh collapsed
    // to null on rejection.
    expect(result.current.data).toBeNull();
  });

  it('populates error when a GIT_CHANGED event triggers a failed refetch', async () => {
    const fetcher = vi
      .fn()
      .mockResolvedValueOnce('first')
      .mockRejectedValueOnce(new Error('git-event-fail'));
    const client = createPathKeyedCache<string>({ fetcher });
    const { result } = renderHook(() =>
      usePathInvalidatedQuery(client, 'k'),
    );
    await waitFor(() => expect(result.current.data).toBe('first'));
    expect(result.current.error).toBeNull();

    await act(async () => {
      await emit('git-changed', { path: 'k' });
    });
    await waitFor(() => expect(result.current.error).toBeInstanceOf(Error));
  });

  it('clears error when a GIT_CHANGED event triggers a successful refetch after a failure', async () => {
    const fetcher = vi
      .fn()
      .mockResolvedValueOnce('first')
      .mockRejectedValueOnce(new Error('fail-1'))
      .mockResolvedValueOnce('third');
    const client = createPathKeyedCache<string>({ fetcher });
    const { result } = renderHook(() =>
      usePathInvalidatedQuery(client, 'k'),
    );
    await waitFor(() => expect(result.current.data).toBe('first'));

    await act(async () => {
      await emit('git-changed', { path: 'k' });
    });
    await waitFor(() => expect(result.current.error).toBeInstanceOf(Error));

    await act(async () => {
      await emit('git-changed', { path: 'k' });
    });
    await waitFor(() => expect(result.current.data).toBe('third'));
    expect(result.current.error).toBeNull();
  });

  it('clears error when the key switches to a different cached key', async () => {
    // First key errors on mount; second key has a cached success.
    const fetcher = vi
      .fn()
      .mockImplementation(async (key: string) => {
        if (key === 'a') throw new Error('a-fail');
        return 'b-value';
      });
    const client = createPathKeyedCache<string>({ fetcher });
    // Pre-seed the cache for 'b' so the second effect takes the cache-hit
    // branch (no fetch, no error possible).
    await client.refresh('b');

    const { result, rerender } = renderHook(
      ({ key }: { key: string }) =>
        usePathInvalidatedQuery(client, key),
      { initialProps: { key: 'a' } },
    );
    await waitFor(() => expect(result.current.error).toBeInstanceOf(Error));

    rerender({ key: 'b' });
    await waitFor(() => expect(result.current.data).toBe('b-value'));
    // New key, cached, no fetch — error must NOT bleed over from 'a'.
    expect(result.current.error).toBeNull();
  });

  it('preserves a legitimate null data result without populating error', async () => {
    // The whole point of the error field: a fetcher that returns `null` or
    // `[]` (a real "known empty" state) must NOT be misreported as an error.
    const client = createPathKeyedCache<string | null>({
      fetcher: vi.fn().mockResolvedValue(null),
    });
    const { result } = renderHook(() =>
      usePathInvalidatedQuery(client, 'k'),
    );
    await waitFor(() => expect(result.current.data).toBeNull());
    // data is null AND no fetch error — these are two distinct states that
    // pre-#342 the hook could not disambiguate.
    expect(result.current.error).toBeNull();
  });
});

// Same set of contracts on the dual-key client — issue #342's
// `error: Error | null` field is a property of the hook's RESULT, not
// of the client shape, so both overloads must populate it the same way.
describe('usePathInvalidatedQuery — error field on dual-key client (issue #342)', () => {
  it('populates error on mount fetch failure when the dual-key overload is used', async () => {
    const boom = new Error('boom');
    const client = createDualKeyCache<number, string>({
      fetcher: vi.fn().mockRejectedValue(boom),
    });
    const { result } = renderHook(() =>
      usePathInvalidatedQuery(client, 7, '/repoA'),
    );
    await waitFor(() => expect(result.current.error).toBe(boom));
    expect(result.current.data).toBeNull();
  });

  it('clears error when the dual-key path switches to a different cached key', async () => {
    const fetcher = vi
      .fn()
      .mockImplementation(async (key: number) => {
        if (key === 1) throw new Error('1-fail');
        return '2-value';
      });
    const client = createDualKeyCache<number, string>({ fetcher });
    await client.refresh(2);

    const { result, rerender } = renderHook(
      ({ key, path }: { key: number; path: string }) =>
        usePathInvalidatedQuery(client, key, path),
      { initialProps: { key: 1, path: '/repoA' } },
    );
    await waitFor(() => expect(result.current.error).toBeInstanceOf(Error));

    rerender({ key: 2, path: '/repoB' });
    await waitFor(() => expect(result.current.data).toBe('2-value'));
    expect(result.current.error).toBeNull();
  });
});

// Issue #392 (follow-up to #349 / PR #390): the mount-refetch, subscribe, and
// focus effects all guard their `.then(setData)` with `signal.aborted`, but
// the manual `refresh()` useCallback does not — it's the only remaining path
// where a stale in-flight fetch's resolution can clobber a new key's state.
describe('usePathInvalidatedQuery — refresh() race on key change (issue #392)', () => {
  it('does not clobber a new key\'s state with a stale refresh() resolution', async () => {
    const resolvers = new Map<number, (value: string) => void>();
    const fetcher = vi.fn(
      (key: number) =>
        new Promise<string>((resolve) => {
          resolvers.set(key, resolve);
        }),
    );
    const client = createDualKeyCache<number, string>({ fetcher });

    const { result, rerender } = renderHook(
      ({ key }: { key: number }) =>
        usePathInvalidatedQuery(client, key, `/repo-${key}`),
      { initialProps: { key: 7 } },
    );

    // Mount fetch for key 7 resolves — settle the initial state.
    await act(async () => {
      resolvers.get(7)?.('seven-initial');
    });
    await waitFor(() => expect(result.current.data).toBe('seven-initial'));

    // User hits refresh() on key 7: invalidate() + a fresh refresh(7) fetch,
    // left in flight (its resolver is captured, not called yet).
    await act(async () => {
      result.current.refresh();
    });
    expect(fetcher).toHaveBeenCalledTimes(2);
    const refreshSevenResolve = resolvers.get(7);

    // User switches to key 8 before the refresh(7) fetch resolves. The
    // mount effect for the new key starts its own fetch, also left pending.
    rerender({ key: 8 });
    expect(fetcher).toHaveBeenCalledWith(8);

    // The stale refresh(7) now resolves. Flush microtasks AND a macrotask
    // tick — client.refresh()'s internal .then/.catch chain needs more than
    // one microtask hop before the hook's own .then(setData) runs (mirrors
    // the flush idiom in use-path-invalidated-query-focus.test.ts).
    await act(async () => {
      refreshSevenResolve?.('seven-refreshed');
      await new Promise((r) => setTimeout(r, 0));
    });

    // Must NOT clobber the still-loading key-8 render with key 7's stale
    // refresh result — state should be untouched (still key 7's last data,
    // since the "needs a fetch" mount-effect branch doesn't reset `data`
    // until its own fetch resolves).
    expect(result.current.data).toBe('seven-initial');
    expect(result.current.loading).toBe(true);

    // Resolve key 8's own fetch and confirm the hook still works normally.
    await act(async () => {
      resolvers.get(8)?.('eight-value');
    });
    await waitFor(() => expect(result.current.data).toBe('eight-value'));
    expect(result.current.loading).toBe(false);
  });
});
