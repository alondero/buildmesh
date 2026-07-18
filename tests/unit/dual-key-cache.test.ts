import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { emit } from '@tauri-apps/api/event';
import {
  createDualKeyCache,
  createPathKeyedCache,
  subscribeGitPathInvalidation,
  resetPathInvalidatedCacheForTests,
} from '../../src/lib/pathInvalidatedCache';

// `pathMatchesGitEvent` is Windows-aware (case + slash normalisation).
// Force `isWindows = true` so the worktree-subdir match branches run
// regardless of the host running the suite.
vi.mock('../../src/lib/platform', () => ({
  isMac: false,
  isWindows: true,
}));

beforeEach(() => {
  // The primitive is a module-level singleton (one bus, many clients).
  // Reset the global state between tests so each test starts with an empty
  // `pathSubscribers` / `busHandlers` and `listenerInstalled = false`.
  resetPathInvalidatedCacheForTests();
});

describe('createDualKeyCache — cache + read', () => {
  it('refresh populates the cache so subsequent read returns the value', async () => {
    const client = createDualKeyCache<number, string>({
      fetcher: vi.fn().mockResolvedValue('hello'),
    });
    expect(client.read(7)).toBeUndefined();
    await client.refresh(7);
    expect(client.read(7)).toBe('hello');
  });

  it('read returns undefined for an uncached key', () => {
    const client = createDualKeyCache<number, string>({
      fetcher: vi.fn(),
    });
    expect(client.read(99)).toBeUndefined();
  });

  it('invalidate evicts the cached value and the next refresh calls fetcher again', async () => {
    const fetcher = vi.fn().mockResolvedValueOnce('first').mockResolvedValueOnce('second');
    const client = createDualKeyCache<number, string>({ fetcher });
    await client.refresh(7);
    expect(client.read(7)).toBe('first');
    client.invalidate(7);
    expect(client.read(7)).toBeUndefined();
    await client.refresh(7);
    expect(client.read(7)).toBe('second');
    expect(fetcher).toHaveBeenCalledTimes(2);
  });

  it('dedupes concurrent refresh calls into a single fetcher invocation', async () => {
    const fetcher = vi.fn().mockResolvedValue('v');
    const client = createDualKeyCache<number, string>({ fetcher });
    await Promise.all([
      client.refresh(7),
      client.refresh(7),
      client.refresh(7),
    ]);
    expect(fetcher).toHaveBeenCalledTimes(1);
  });
});

describe('createDualKeyCache — null-as-value', () => {
  it('treats a resolved null as a real cached value (not uncached)', async () => {
    const client = createDualKeyCache<number, string | null>({
      fetcher: vi.fn().mockResolvedValue(null),
    });
    expect(client.read(7)).toBeUndefined();
    await client.refresh(7);
    expect(client.read(7)).toBeNull();
  });
});

describe('createDualKeyCache — error handling', () => {
  it('resolves refresh to null on fetcher rejection and leaves the cache empty', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const client = createDualKeyCache<number, string>({
      fetcher: vi.fn().mockRejectedValue(new Error('boom')),
    });
    await expect(client.refresh(7)).resolves.toBeNull();
    expect(client.read(7)).toBeUndefined();
    expect(warn).toHaveBeenCalledTimes(1);
    warn.mockRestore();
  });

  it('uses the `name` option as the log tag in console.warn', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const client = createDualKeyCache<number, string>({
      fetcher: vi.fn().mockRejectedValue(new Error('boom')),
      name: 'useOpenPr',
    });
    await client.refresh(7);
    expect(warn).toHaveBeenCalledWith(
      'useOpenPr: fetch failed for key',
      7,
      expect.any(Error),
    );
    warn.mockRestore();
  });
});

describe('createDualKeyCache — bus + subscribeByPath', () => {
  it('notifies subscribers on a matching GIT_CHANGED event (subscribeByPath)', async () => {
    // The KEY is an id (7), the PATH is a string. The method is
    // `subscribeByPath(key, path, cb)` — the dual-key shape.
    const client = createDualKeyCache<number, string>({ fetcher: vi.fn() });
    const cb = vi.fn();
    client.subscribeByPath(7, '/repo', cb);
    await emit('git-changed', { path: '/repo' });
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it('matches a worktree subdir against a mesh-root subscription (issue #304)', async () => {
    const client = createDualKeyCache<number, string>({ fetcher: vi.fn() });
    const cb = vi.fn();
    client.subscribeByPath(7, '/repo', cb);
    await emit('git-changed', { path: '/repo/.claude/worktrees/gentle-fox' });
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it('keeps different keys on the same path independent — both are notified', async () => {
    const client = createDualKeyCache<number, string>({ fetcher: vi.fn() });
    const cb7 = vi.fn();
    const cb8 = vi.fn();
    client.subscribeByPath(7, '/repo', cb7);
    client.subscribeByPath(8, '/repo', cb8);
    await emit('git-changed', { path: '/repo' });
    expect(cb7).toHaveBeenCalledTimes(1);
    expect(cb8).toHaveBeenCalledTimes(1);
  });

  it('unsubscribe stops further notifications for that subscriber', async () => {
    const client = createDualKeyCache<number, string>({ fetcher: vi.fn() });
    const cb = vi.fn();
    const unsubscribe = client.subscribeByPath(7, '/repo', cb);
    await emit('git-changed', { path: '/repo' });
    expect(cb).toHaveBeenCalledTimes(1);
    unsubscribe();
    await emit('git-changed', { path: '/repo' });
    expect(cb).toHaveBeenCalledTimes(1);  // unchanged
  });
});

describe('createDualKeyCache — bus invalidates cache before notify', () => {
  it('evicts the cached value for the matching key before invoking the subscriber callback', async () => {
    const client = createDualKeyCache<number, string>({
      fetcher: vi.fn().mockResolvedValue('first'),
    });
    await client.refresh(7);
    expect(client.read(7)).toBe('first');

    client.subscribeByPath(7, '/repo', () => {
      // The bus handler runs first, evicting the cache entry.
    });
    await emit('git-changed', { path: '/repo' });
    expect(client.read(7)).toBeUndefined();
  });
});

describe('createDualKeyCache — bus dispatch is scoped to the owning client', () => {
  it('a sibling dual-key client with the same id is NOT wiped', async () => {
    // Footgun 1 (per the memory): a naive "iterate all clients and wipe
    // the entry for key === 7" implementation would also nuke clientB's
    // cache when an event for /repoA fires. The primitive scopes
    // dispatch via the subscriber's clientId so only the owning client
    // is touched.
    const clientA = createDualKeyCache<number, string>({
      fetcher: vi.fn().mockResolvedValue('value-A'),
    });
    const clientB = createDualKeyCache<number, string>({
      fetcher: vi.fn().mockResolvedValue('value-B'),
    });
    await clientA.refresh(7);
    await clientB.refresh(7);
    expect(clientA.read(7)).toBe('value-A');
    expect(clientB.read(7)).toBe('value-B');

    const cbA = vi.fn();
    clientA.subscribeByPath(7, '/repoA', cbA);

    // Fire an event for /repoA — only clientA is subscribed there.
    await emit('git-changed', { path: '/repoA' });
    expect(cbA).toHaveBeenCalledTimes(1);
    // clientB was not subscribed to /repoA, so its cache for 7 is intact.
    expect(clientB.read(7)).toBe('value-B');
  });

  it('a path-keyed client on the same path is NOT wiped by a dual-key event', async () => {
    // Cross-factory isolation: a `createPathKeyedCache` client and a
    // `createDualKeyCache` client both registered on /repoA must each
    // only have their OWN cache entry wiped when the bus fires.
    const pathClient = createPathKeyedCache<string>({
      fetcher: vi.fn().mockResolvedValue('path-value'),
    });
    const dualClient = createDualKeyCache<number, string>({
      fetcher: vi.fn().mockResolvedValue('dual-value'),
    });
    await pathClient.refresh('/repoA');
    await dualClient.refresh(7);
    expect(pathClient.read('/repoA')).toBe('path-value');
    expect(dualClient.read(7)).toBe('dual-value');

    const cbPath = vi.fn();
    const cbDual = vi.fn();
    pathClient.subscribe('/repoA', cbPath);
    dualClient.subscribeByPath(7, '/repoA', cbDual);

    await emit('git-changed', { path: '/repoA' });
    expect(cbPath).toHaveBeenCalledTimes(1);
    expect(cbDual).toHaveBeenCalledTimes(1);
    // Each handler evicts its OWN client's cache entry — neither touches
    // the other.
    expect(pathClient.read('/repoA')).toBeUndefined();
    expect(dualClient.read(7)).toBeUndefined();
  });

  it('a path-keyed client and a callback-only subscriber on the same path both fire (issue #345)', async () => {
    // The bus must dispatch the path-keyed handler via its own clientId
    // AND the noop handler for the callback subscriber — same dispatch
    // path the old single-factory API pinned.
    const pathClient = createPathKeyedCache<string>({
      fetcher: vi.fn().mockResolvedValue('value-A'),
    });
    await pathClient.refresh('/repoA');
    expect(pathClient.read('/repoA')).toBe('value-A');

    const cb = vi.fn();
    pathClient.subscribe('/repoA', () => {});
    subscribeGitPathInvalidation('/repoA', cb);

    await emit('git-changed', { path: '/repoA' });
    expect(cb).toHaveBeenCalledTimes(1);
    expect(pathClient.read('/repoA')).toBeUndefined();
  });
});

describe('minRefetchIntervalMs — freshness window on bus invalidation', () => {
  // Fake timers drive Date.now() so the window boundary is exact.
  afterEach(() => {
    vi.useRealTimers();
  });

  it('a matching event inside the window neither evicts nor notifies immediately', async () => {
    vi.useFakeTimers();
    const client = createDualKeyCache<number, string>({
      fetcher: vi.fn().mockResolvedValue('fresh'),
      minRefetchIntervalMs: 30_000,
    });
    await client.refresh(7);
    const cb = vi.fn();
    client.subscribeByPath(7, '/repo', cb);

    vi.advanceTimersByTime(29_999);
    await emit('git-changed', { path: '/repo' });

    expect(cb).not.toHaveBeenCalled();
    expect(client.read(7)).toBe('fresh');

    // …but the settled state still lands: a trailing evict+notify fires
    // when the window expires (1ms later), so a burst that ends inside
    // the window can never leave the panel permanently stale.
    vi.advanceTimersByTime(1);
    expect(cb).toHaveBeenCalledTimes(1);
    expect(client.read(7)).toBeUndefined();
  });

  it('a burst of suppressed events coalesces into ONE trailing notify', async () => {
    vi.useFakeTimers();
    const client = createDualKeyCache<number, string>({
      fetcher: vi.fn().mockResolvedValue('fresh'),
      minRefetchIntervalMs: 30_000,
    });
    await client.refresh(7);
    const cb = vi.fn();
    client.subscribeByPath(7, '/repo', cb);

    // Five events, all inside the window — an agent streaming edits.
    for (let i = 0; i < 5; i++) {
      vi.advanceTimersByTime(1_000);
      await emit('git-changed', { path: '/repo' });
    }
    expect(cb).not.toHaveBeenCalled();

    // The single trailing refetch fires at window expiry (30s after the
    // fetch, i.e. 25s after the last event) — once, not five times.
    vi.advanceTimersByTime(25_000);
    expect(cb).toHaveBeenCalledTimes(1);
    expect(client.read(7)).toBeUndefined();
  });

  it('a successful refresh cancels the pending trailing refetch', async () => {
    vi.useFakeTimers();
    const client = createDualKeyCache<number, string>({
      fetcher: vi.fn().mockResolvedValue('fresh'),
      minRefetchIntervalMs: 30_000,
    });
    await client.refresh(7);
    const cb = vi.fn();
    client.subscribeByPath(7, '/repo', cb);

    vi.advanceTimersByTime(1_000);
    await emit('git-changed', { path: '/repo' }); // suppressed → trailing armed

    // A manual refresh (user clicked Refresh) already delivers the state
    // the suppressed event described — the trailing timer must be
    // cancelled, or it would evict this fresh value one window later.
    await client.refresh(7);
    vi.advanceTimersByTime(60_000);
    expect(cb).not.toHaveBeenCalled();
    expect(client.read(7)).toBe('fresh');
  });

  it('a matching event after the window evicts and notifies as usual', async () => {
    vi.useFakeTimers();
    const client = createDualKeyCache<number, string>({
      fetcher: vi.fn().mockResolvedValue('fresh'),
      minRefetchIntervalMs: 30_000,
    });
    await client.refresh(7);
    const cb = vi.fn();
    client.subscribeByPath(7, '/repo', cb);

    vi.advanceTimersByTime(30_000);
    await emit('git-changed', { path: '/repo' });

    expect(cb).toHaveBeenCalledTimes(1);
    expect(client.read(7)).toBeUndefined();
  });

  it('a key that has never fetched successfully is not gated', async () => {
    vi.useFakeTimers();
    // No refresh() — there is no lastFetchedAt stamp, so the event must
    // notify (the subscriber may be waiting for its first load trigger).
    const client = createDualKeyCache<number, string>({
      fetcher: vi.fn(),
      minRefetchIntervalMs: 30_000,
    });
    const cb = vi.fn();
    client.subscribeByPath(7, '/repo', cb);

    await emit('git-changed', { path: '/repo' });
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it('manual invalidate() is NOT gated — an explicit eviction always wins', async () => {
    vi.useFakeTimers();
    const client = createDualKeyCache<number, string>({
      fetcher: vi.fn().mockResolvedValue('fresh'),
      minRefetchIntervalMs: 30_000,
    });
    await client.refresh(7);
    expect(client.read(7)).toBe('fresh');

    client.invalidate(7); // inside the window
    expect(client.read(7)).toBeUndefined();
  });

  it('invalidate() clears the freshness stamp — a bus event after manual invalidation notifies', async () => {
    vi.useFakeTimers();
    const client = createDualKeyCache<number, string>({
      fetcher: vi.fn().mockResolvedValue('fresh'),
      minRefetchIntervalMs: 30_000,
    });
    await client.refresh(7);
    const cb = vi.fn();
    client.subscribeByPath(7, '/repo', cb);

    // Manual invalidation says "no longer authoritative" — the window must
    // not keep suppressing bus notifications while the cache sits empty.
    client.invalidate(7);
    await emit('git-changed', { path: '/repo' });
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it('createPathKeyedCache honours the window too', async () => {
    vi.useFakeTimers();
    const client = createPathKeyedCache<string>({
      fetcher: vi.fn().mockResolvedValue('fresh'),
      minRefetchIntervalMs: 30_000,
    });
    await client.refresh('/repo');
    const cb = vi.fn();
    client.subscribe('/repo', cb);

    await emit('git-changed', { path: '/repo' });
    expect(cb).not.toHaveBeenCalled();
    expect(client.read('/repo')).toBe('fresh');

    // Advancing past the window fires the trailing refetch armed by the
    // suppressed event above (one notify)…
    vi.advanceTimersByTime(30_000);
    expect(cb).toHaveBeenCalledTimes(1);
    expect(client.read('/repo')).toBeUndefined();

    // …and with the stamp cleared, the next bus event notifies normally.
    await emit('git-changed', { path: '/repo' });
    expect(cb).toHaveBeenCalledTimes(2);
    expect(client.read('/repo')).toBeUndefined();
  });

  it('defaults to 0 — every matching event evicts (original behaviour)', async () => {
    vi.useFakeTimers();
    const client = createDualKeyCache<number, string>({
      fetcher: vi.fn().mockResolvedValue('fresh'),
    });
    await client.refresh(7);
    const cb = vi.fn();
    client.subscribeByPath(7, '/repo', cb);

    await emit('git-changed', { path: '/repo' }); // zero ms later
    expect(cb).toHaveBeenCalledTimes(1);
    expect(client.read(7)).toBeUndefined();
  });
});
