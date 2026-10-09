/**
 * Retention and inactive-freshness contract for the path-invalidated
 * cache (issue #2017).
 *
 * The primitive retains `known` / `values` / `errors` / `lastFetchedAt`
 * per queried key. Before this suite the only removal paths were a bus
 * eviction or a manual `invalidate`, so unsubscribing left the entry
 * behind forever and an inactive key could serve a value that no live
 * subscriber was around to invalidate.
 *
 * What these cases pin:
 *   - settled cardinality after thousands of query/switch/delete cycles
 *     (the cap), measured through the PUBLIC API — no internals exported
 *   - active keys are pinned against the cap and the TTL
 *   - the revisit contract, both sides: a quick remount is still a cache
 *     hit, a revisit past the TTL refetches
 *   - a completion that was already in flight when the key was evicted
 *     cannot resurrect the entry (the `pending`-identity fence)
 *   - `null` vs `undefined` semantics survive expiry
 *   - errors are per-key retained state and expire on the same clock
 *   - dual-key clients (nodeId / meshId keyed) get the same policy
 */

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { emit } from '@tauri-apps/api/event';
import {
  createPathKeyedCache,
  createDualKeyCache,
  resetPathInvalidatedCacheForTests,
} from '../../src/lib/pathInvalidatedCache';

// `pathMatchesGitEvent` is Windows-aware (case + slash normalisation);
// pin it so the match branches run identically on every host.
vi.mock('../../src/lib/platform', () => ({
  isMac: false,
  isWindows: true,
}));

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

describe('inactive entry cap (issue #2017)', () => {
  it('bounds settled entries after thousands of query/switch/delete cycles', async () => {
    const client = createPathKeyedCache<string>({
      fetcher: (key) => Promise.resolve(`value:${key}`),
      // Small cap so the bound is provable without a 200-entry setup.
      maxInactiveEntries: 50,
    });

    // 2,000 keys, each queried then unsubscribed — the shape of a long
    // session walking repo paths, deleting worktrees included.
    for (let i = 0; i < 2_000; i++) {
      const key = `/worktree-${i}`;
      const unsubscribe = client.subscribe(key, () => {});
      await client.refresh(key);
      unsubscribe();
    }

    // The cap is observable through `read`: the most recent 50 keys are
    // retained, the oldest 1,950 are gone. Count the live entries rather
    // than trusting a single probe.
    let retained = 0;
    for (let i = 0; i < 2_000; i++) {
      if (client.read(`/worktree-${i}`) !== undefined) retained++;
    }
    expect(retained).toBe(50);
    // The survivors are the most recently released, not an arbitrary set.
    expect(client.read('/worktree-1999')).toBe('value:/worktree-1999');
    expect(client.read('/worktree-0')).toBeUndefined();
    expect(client.read('/worktree-1949')).toBeUndefined();
    expect(client.read('/worktree-1950')).toBe('value:/worktree-1950');
  });

  it('pins a key with a live subscriber while inactive entries are evicted', async () => {
    const client = createPathKeyedCache<string>({
      fetcher: (key) => Promise.resolve(`value:${key}`),
      maxInactiveEntries: 5,
    });

    // A live subscriber on a key that also has other keys competing
    // against it. It must survive churn that would otherwise evict it.
    const liveKey = '/live';
    client.subscribe(liveKey, () => {});
    await client.refresh(liveKey);

    for (let i = 0; i < 200; i++) {
      const key = `/churn-${i}`;
      const unsubscribe = client.subscribe(key, () => {});
      await client.refresh(key);
      unsubscribe();
      // Read mid-flight so the pinned value is never merely absent.
      expect(client.read(liveKey)).toBe('value:/live');
    }

    // 200 churn cycles against a cap of 5 — the pinned key is untouched.
    expect(client.read(liveKey)).toBe('value:/live');
  });

  it('keeps the last subscriber on a shared key alive when a sibling unmounts', async () => {
    const client = createPathKeyedCache<string>({
      fetcher: (key) => Promise.resolve('value'),
      maxInactiveEntries: 1,
    });
    const first = client.subscribe('/shared', () => {});
    const second = client.subscribe('/shared', () => {});
    await client.refresh('/shared');

    // Churn enough to fill and overfill the cap while `/shared` still has
    // a live subscriber through `second`.
    for (let i = 0; i < 20; i++) {
      const key = `/other-${i}`;
      const unsubscribe = client.subscribe(key, () => {});
      await client.refresh(key);
      unsubscribe();
    }

    expect(client.read('/shared')).toBe('value');
    first();
    // Only now, with `second` still mounted, the entry is still pinned.
    expect(client.read('/shared')).toBe('value');
    second();
    // After the final unsubscribe it becomes evictable like any other.
    for (let i = 20; i < 40; i++) {
      const key = `/after-${i}`;
      const unsubscribe = client.subscribe(key, () => {});
      await client.refresh(key);
      unsubscribe();
    }
    expect(client.read('/shared')).toBeUndefined();
  });

  it('ships the documented defaults when the options are omitted', async () => {
    // Every other case overrides the bounds so the ceiling is provable in
    // a few cycles. That leaves the SHIPPED defaults unpinned, and
    // `docs/development/git-query-cache.md` states both numbers. This case
    // is what stops the doc and the code drifting apart silently.
    vi.useFakeTimers();
    const client = createPathKeyedCache<string>({
      fetcher: (key) => Promise.resolve(`value:${key}`),
    });

    // 260 inactive entries against the documented default cap of 200.
    for (let i = 0; i < 260; i++) {
      const off = client.subscribe(`/default-${i}`, () => {});
      await client.refresh(`/default-${i}`);
      off();
    }
    let retained = 0;
    for (let i = 0; i < 260; i++) {
      if (client.read(`/default-${i}`) !== undefined) retained++;
    }
    expect(retained).toBe(200);
    expect(client.read('/default-259')).toBe('value:/default-259');
    expect(client.read('/default-59')).toBeUndefined();

    // And the documented default TTL of 5 minutes: still served just
    // inside it, retired just past it.
    const off = client.subscribe('/ttl-default', () => {});
    await client.refresh('/ttl-default');
    off();
    await vi.advanceTimersByTimeAsync(4 * 60_000);
    expect(client.read('/ttl-default')).toBe('value:/ttl-default');
    await vi.advanceTimersByTimeAsync(2 * 60_000);
    expect(client.read('/ttl-default')).toBeUndefined();
  });

  it('releases a key exactly once when its unsubscribe closure runs twice', async () => {
    const client = createPathKeyedCache<string>({
      fetcher: (key) => Promise.resolve('value'),
      maxInactiveEntries: 2,
    });
    const unsubscribe = client.subscribe('/repo', () => {});
    await client.refresh('/repo');

    unsubscribe();
    unsubscribe();

    // A double release would drive the refcount negative and unpin the
    // key permanently; 10 further cycles against a cap of 2 prove it was
    // released once and really is evictable.
    for (let i = 0; i < 10; i++) {
      const key = `/cycle-${i}`;
      const off = client.subscribe(key, () => {});
      await client.refresh(key);
      off();
    }
    expect(client.read('/repo')).toBeUndefined();
  });
});

describe('revisit freshness after offscreen changes (issue #2017)', () => {
  it('serves the cached value on a revisit inside the TTL window', async () => {
    vi.useFakeTimers();
    const fetcher = vi.fn((key: string) => Promise.resolve(`value:${key}`));
    const client = createPathKeyedCache<string>({ fetcher, inactiveEntryTtlMs: 60_000 });

    const unsubscribe = client.subscribe('/repo', () => {});
    await client.refresh('/repo');
    unsubscribe();

    // Well inside the window: the remount reads without a refetch.
    await vi.advanceTimersByTimeAsync(30_000);
    expect(client.read('/repo')).toBe('value:/repo');
    expect(fetcher).toHaveBeenCalledTimes(1);
  });

  it('retires a stale inactive entry so a revisit after offscreen git changes refetches', async () => {
    vi.useFakeTimers();
    const fetcher = vi.fn()
      .mockResolvedValueOnce('stale snapshot')
      .mockResolvedValueOnce('fresh snapshot');
    const client = createPathKeyedCache<string>({ fetcher, inactiveEntryTtlMs: 60_000 });

    const unsubscribe = client.subscribe('/repo', () => {});
    await client.refresh('/repo');
    unsubscribe();

    // The key is off screen — `GIT_CHANGED` reaches nobody, so this
    // event cannot evict anything. The entry is now silently stale.
    await vi.advanceTimersByTimeAsync(10_000);
    await emit('git-changed', { path: '/repo' });
    expect(client.read('/repo')).toBe('stale snapshot');

    // Past the TTL the revisit must not serve that value.
    await vi.advanceTimersByTimeAsync(60_000);
    expect(client.read('/repo')).toBeUndefined();
    expect(await client.refresh('/repo')).toBe('fresh snapshot');
    expect(fetcher).toHaveBeenCalledTimes(2);
  });

  it('never expires a key that still has a live subscriber, however much time passes', async () => {
    vi.useFakeTimers();
    const client = createPathKeyedCache<string>({
      fetcher: (key) => Promise.resolve('value'),
      inactiveEntryTtlMs: 1_000,
      maxInactiveEntries: 1,
    });
    client.subscribe('/repo', () => {});
    await client.refresh('/repo');

    await vi.advanceTimersByTimeAsync(60 * 60_000);
    expect(client.read('/repo')).toBe('value');
  });

  it('never expires a key with a live subscriber even while churn fills the cap', async () => {
    vi.useFakeTimers();
    const client = createPathKeyedCache<string>({
      fetcher: (key) => Promise.resolve(`value:${key}`),
      inactiveEntryTtlMs: 1_000,
      maxInactiveEntries: 2,
    });
    client.subscribe('/live', () => {});
    await client.refresh('/live');

    for (let i = 0; i < 50; i++) {
      const off = client.subscribe(`/churn-${i}`, () => {});
      await client.refresh(`/churn-${i}`);
      off();
      await vi.advanceTimersByTimeAsync(5_000);
      expect(client.read('/live')).toBe('value:/live');
    }
  });
});

describe('late completion fencing across eviction (issue #2017)', () => {
  it('does not resurrect an entry whose in-flight completion lands after eviction', async () => {
    const pending = deferred();
    // Only `/repo` stays in flight; the churn key resolves immediately so
    // the eviction happens while `/repo`'s fetch is genuinely unfinished.
    const client = createPathKeyedCache<string>({
      fetcher: (key) => (key === '/repo' ? pending.promise : Promise.resolve(`value:${key}`)),
      maxInactiveEntries: 1,
    });
    const unsubscribe = client.subscribe('/repo', () => {});
    const refresh = client.refresh('/repo');
    unsubscribe();

    // One churn cycle over the cap evicts `/repo` while its fetch is
    // still in flight.
    const churnOff = client.subscribe('/churn', () => {});
    await client.refresh('/churn');
    churnOff();
    expect(client.read('/repo')).toBeUndefined();

    // The completion now lands against an evicted key.
    pending.resolve('late value');
    expect(await refresh).toBeNull();
    expect(client.read('/repo')).toBeUndefined();
  });

  it('does not resurrect an error when a rejected completion lands after eviction', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    const pending = deferred();
    const client = createPathKeyedCache<string>({
      fetcher: (key) => (key === '/repo' ? pending.promise : Promise.resolve(`value:${key}`)),
      maxInactiveEntries: 1,
    });
    const unsubscribe = client.subscribe('/repo', () => {});
    const refresh = client.refresh('/repo');
    unsubscribe();

    const churnOff = client.subscribe('/churn', () => {});
    await client.refresh('/churn');
    churnOff();

    pending.reject(new Error('late failure'));
    expect(await refresh).toBeNull();
    expect(client.lastError('/repo')).toBeNull();
    expect(client.read('/repo')).toBeUndefined();
  });

  it('lets a revisit that starts a new request win over a completion fenced by eviction', async () => {
    const stale = deferred();
    const fresh = deferred();
    let repoCalls = 0;
    const client = createPathKeyedCache<string>({
      fetcher: (key) => {
        if (key !== '/repo') return Promise.resolve(`value:${key}`);
        repoCalls += 1;
        return repoCalls === 1 ? stale.promise : fresh.promise;
      },
      maxInactiveEntries: 1,
    });
    const unsubscribe = client.subscribe('/repo', () => {});
    const staleRefresh = client.refresh('/repo');
    unsubscribe();

    // Evict `/repo` while its first request is still in flight...
    const churnOff = client.subscribe('/churn', () => {});
    await client.refresh('/churn');
    churnOff();
    expect(client.read('/repo')).toBeUndefined();

    // ...then revisit. Because the eviction dropped the pending slot, the
    // revisit starts a genuinely NEW request (second call to the
    // fetcher) instead of adopting the fenced one.
    const revisit = client.refresh('/repo');
    expect(repoCalls).toBe(2);

    stale.resolve('obsolete');
    // Flush the fenced completion. It must not publish its value: the key
    // was evicted, so there is nothing for it to write into.
    await Promise.resolve();
    await Promise.resolve();
    expect(client.read('/repo')).toBeUndefined();

    fresh.resolve('current');
    expect(await revisit).toBe('current');
    // The fenced caller adopts the current request, matching the existing
    // supersession rule — it never surfaces the obsolete value.
    expect(await staleRefresh).toBe('current');
    expect(client.read('/repo')).toBe('current');
  });
});

describe('retained-state semantics under retention (issue #2017)', () => {
  it('keeps cached null distinct from uncached after a revisit past the TTL', async () => {
    vi.useFakeTimers();
    const client = createPathKeyedCache<string>({
      fetcher: () => Promise.resolve(null),
      inactiveEntryTtlMs: 60_000,
    });

    const unsubscribe = client.subscribe('/repo', () => {});
    await client.refresh('/repo');
    unsubscribe();

    // Inside the TTL a cached `null` is a real, served answer.
    await vi.advanceTimersByTimeAsync(10_000);
    expect(client.read('/repo')).toBeNull();

    // Past the TTL it retires to `undefined` (uncached), so the caller
    // refetches rather than treating the expired null as authoritative.
    await vi.advanceTimersByTimeAsync(60_000);
    expect(client.read('/repo')).toBeUndefined();
  });

  it('expires a recorded error on the same clock as its value', async () => {
    vi.useFakeTimers();
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    const client = createPathKeyedCache<string>({
      fetcher: () => Promise.reject(new Error('fetch failed')),
      inactiveEntryTtlMs: 60_000,
    });

    const unsubscribe = client.subscribe('/repo', () => {});
    await client.refresh('/repo');
    unsubscribe();

    expect(client.lastError('/repo')?.message).toBe('fetch failed');
    await vi.advanceTimersByTimeAsync(60_000);
    expect(client.lastError('/repo')).toBeNull();
    expect(client.read('/repo')).toBeUndefined();
  });

  it('does not let one key failure leak to another key evicted by the cap', async () => {
    vi.spyOn(console, 'warn').mockImplementation(() => {});
    const client = createPathKeyedCache<string>({
      fetcher: (key) => (key === '/bad' ? Promise.reject(new Error('bad')) : Promise.resolve('ok')),
      // Cap 3: `/bad` and `/good` settle into the LRU, then the first two
      // released filler keys push it to 4 entries and evict the OLDEST —
      // `/bad`, not `/good`.
      maxInactiveEntries: 3,
    });

    const badOff = client.subscribe('/bad', () => {});
    await client.refresh('/bad');
    badOff();
    const goodOff = client.subscribe('/good', () => {});
    await client.refresh('/good');
    goodOff();

    // Errors are per-key: the failure is visible for `/bad` only.
    expect(client.lastError('/bad')?.message).toBe('bad');
    expect(client.lastError('/good')).toBeNull();
    expect(client.read('/good')).toBe('ok');

    for (const key of ['/filler-0', '/filler-1']) {
      const off = client.subscribe(key, () => {});
      await client.refresh(key);
      off();
    }

    // `/bad` was the least-recently-inactive, so it went first — and the
    // sibling's clean state is untouched by its eviction.
    expect(client.lastError('/bad')).toBeNull();
    expect(client.read('/good')).toBe('ok');
    expect(client.lastError('/good')).toBeNull();
  });

  it('counts a never-subscribed key against the cap so imperative refreshes cannot grow without bound', async () => {
    const client = createPathKeyedCache<string>({
      fetcher: (key) => Promise.resolve(`value:${key}`),
      maxInactiveEntries: 10,
    });

    // No subscribe() anywhere — the imperative-refresh shape.
    for (let i = 0; i < 500; i++) {
      await client.refresh(`/imperative-${i}`);
    }

    let retained = 0;
    for (let i = 0; i < 500; i++) {
      if (client.read(`/imperative-${i}`) !== undefined) retained++;
    }
    expect(retained).toBe(10);
    expect(client.read('/imperative-499')).toBe('value:/imperative-499');
    expect(client.read('/imperative-0')).toBeUndefined();
  });

  it('drops the LRU stamp when an explicit invalidate empties a key', async () => {
    const client = createPathKeyedCache<string>({
      fetcher: (key) => Promise.resolve(`value:${key}`),
      maxInactiveEntries: 4,
    });

    const first = client.subscribe('/repo', () => {});
    await client.refresh('/repo');
    first();
    client.invalidate('/repo');

    // The key holds nothing now, so it must not occupy one of the four
    // LRU slots — four later keys still fit.
    for (let i = 0; i < 4; i++) {
      const off = client.subscribe(`/fill-${i}`, () => {});
      await client.refresh(`/fill-${i}`);
      off();
    }
    for (let i = 0; i < 4; i++) {
      expect(client.read(`/fill-${i}`)).toBe(`value:/fill-${i}`);
    }
  });
});

describe('dual-key clients get the same policy (issue #2017)', () => {
  it('bounds an entity-keyed cache across deleted entities', async () => {
    const client = createDualKeyCache<number, string>({
      fetcher: (meshId) => Promise.resolve(`health:${meshId}`),
      maxInactiveEntries: 20,
    });

    // Mesh ids churn the way a session that creates and deletes meshes
    // produces them — including ids that never come back.
    for (let id = 1; id <= 1_000; id++) {
      const off = client.subscribeByPath(id, `/mesh-${id}`, () => {});
      await client.refresh(id);
      off();
    }

    let retained = 0;
    for (let id = 1; id <= 1_000; id++) {
      if (client.read(id) !== undefined) retained++;
    }
    expect(retained).toBe(20);
    expect(client.read(1_000)).toBe('health:1000');
    expect(client.read(1)).toBeUndefined();
  });

  it('pins an entity whose subscriber is live and expires it once released', async () => {
    vi.useFakeTimers();
    const client = createDualKeyCache<number, string>({
      fetcher: (nodeId) => Promise.resolve(`pr:${nodeId}`),
      inactiveEntryTtlMs: 30_000,
    });

    const off = client.subscribeByPath(7, '/repo/node-7', () => {});
    await client.refresh(7);

    await vi.advanceTimersByTimeAsync(120_000);
    expect(client.read(7)).toBe('pr:7');

    off();
    await vi.advanceTimersByTimeAsync(30_000);
    expect(client.read(7)).toBeUndefined();
  });
});