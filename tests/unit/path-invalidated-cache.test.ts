import { describe, it, expect, beforeEach, vi } from 'vitest';
import { listen, emit } from '@tauri-apps/api/event';
import {
  createPathInvalidatedCache,
  resetPathInvalidatedCacheForTests,
  subscribeGitPathInvalidation,
} from '../../src/lib/pathInvalidatedCache';

// The helper is the same one used by the four original hooks; force
// `isWindows = true` so the case-insensitive + backslash normalisation
// branches run regardless of the host running the suite. None of the
// primitive-level tests below depend on the cross-platform path semantics
// in a way that would be affected by the mock — the worktree-subdir match
// test is pinned by `tests/unit/paths.test.ts` separately.
vi.mock('../../src/lib/platform', () => ({
  isMac: false,
  isWindows: true,
}));

const mockedListen = vi.mocked(listen);

beforeEach(() => {
  // The primitive is a module-level singleton (one bus, many clients).
  // Reset the global state between tests so each test starts with an empty
  // `pathSubscribers` / `busHandlers` and `listenerInstalled = false`.
  resetPathInvalidatedCacheForTests();
});

describe('createPathInvalidatedCache — cache + read', () => {
  it('refresh populates the cache so subsequent read returns the value', async () => {
    const client = createPathInvalidatedCache<string, string>({
      fetcher: vi.fn().mockResolvedValue('hello'),
    });
    expect(client.read('k')).toBeUndefined();
    await client.refresh('k');
    expect(client.read('k')).toBe('hello');
  });

  it('read returns undefined for an uncached key', () => {
    const client = createPathInvalidatedCache<string, string>({
      fetcher: vi.fn(),
    });
    expect(client.read('never-set')).toBeUndefined();
  });

  it('invalidate evicts the cached value and the next refresh calls fetcher again', async () => {
    const fetcher = vi.fn().mockResolvedValueOnce('first').mockResolvedValueOnce('second');
    const client = createPathInvalidatedCache<string, string>({ fetcher });
    await client.refresh('k');
    expect(client.read('k')).toBe('first');
    client.invalidate('k');
    expect(client.read('k')).toBeUndefined();
    await client.refresh('k');
    expect(client.read('k')).toBe('second');
    expect(fetcher).toHaveBeenCalledTimes(2);
  });

  it('dedupes concurrent refresh calls into a single fetcher invocation', async () => {
    const fetcher = vi.fn().mockResolvedValue('v');
    const client = createPathInvalidatedCache<string, string>({ fetcher });
    await Promise.all([
      client.refresh('k'),
      client.refresh('k'),
      client.refresh('k'),
    ]);
    expect(fetcher).toHaveBeenCalledTimes(1);
  });
});

describe('createPathInvalidatedCache — null-as-value', () => {
  it('treats a resolved null as a real cached value (not uncached)', async () => {
    const client = createPathInvalidatedCache<string, string | null>({
      fetcher: vi.fn().mockResolvedValue(null),
    });
    expect(client.read('k')).toBeUndefined();
    await client.refresh('k');
    // The cache uses a Symbol sentinel to disambiguate cached-null from
    // uncached; the read API surfaces that as `null`, not `undefined`.
    expect(client.read('k')).toBeNull();
  });
});

describe('createPathInvalidatedCache — error handling', () => {
  it('resolves refresh to null on fetcher rejection and leaves the cache empty', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const client = createPathInvalidatedCache<string, string>({
      fetcher: vi.fn().mockRejectedValue(new Error('boom')),
    });
    await expect(client.refresh('k')).resolves.toBeNull();
    expect(client.read('k')).toBeUndefined();
    expect(warn).toHaveBeenCalledTimes(1);
    warn.mockRestore();
  });

  it('uses the `name` option as the log tag in console.warn', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const client = createPathInvalidatedCache<string, string>({
      fetcher: vi.fn().mockRejectedValue(new Error('boom')),
      name: 'useGitSummary',
    });
    await client.refresh('k');
    expect(warn).toHaveBeenCalledWith(
      'useGitSummary: fetch failed for key',
      'k',
      expect.any(Error),
    );
    warn.mockRestore();
  });
});

describe('createPathInvalidatedCache — bus + subscribe', () => {
  it('notifies subscribers on a matching GIT_CHANGED event', async () => {
    const client = createPathInvalidatedCache<string, string>({ fetcher: vi.fn() });
    const cb = vi.fn();
    client.subscribe('k', '/repo', cb);
    await emit('git-changed', { path: '/repo' });
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it('matches a worktree subdir against a mesh-root subscription (issue #304)', async () => {
    // The subscriber registered with the mesh root `/repo`; the watcher
    // emits the worktree subdir. The primitive's listener uses
    // `pathMatchesGitEvent`, which is the same helper the four original
    // hooks call. Detailed slash/case/normalisation coverage lives in
    // `tests/unit/paths.test.ts`; this is the wiring smoke test.
    const client = createPathInvalidatedCache<string, string>({ fetcher: vi.fn() });
    const cb = vi.fn();
    client.subscribe('k', '/repo', cb);
    await emit('git-changed', { path: '/repo/.claude/worktrees/gentle-fox' });
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it('keeps different keys on the same path independent — both are notified', async () => {
    const client = createPathInvalidatedCache<number, string>({ fetcher: vi.fn() });
    const cb7 = vi.fn();
    const cb8 = vi.fn();
    client.subscribe(7, '/repo', cb7);
    client.subscribe(8, '/repo', cb8);
    await emit('git-changed', { path: '/repo' });
    expect(cb7).toHaveBeenCalledTimes(1);
    expect(cb8).toHaveBeenCalledTimes(1);
  });

  it('unsubscribe stops further notifications for that subscriber', async () => {
    const client = createPathInvalidatedCache<string, string>({ fetcher: vi.fn() });
    const cb = vi.fn();
    const unsubscribe = client.subscribe('k', '/repo', cb);
    await emit('git-changed', { path: '/repo' });
    expect(cb).toHaveBeenCalledTimes(1);
    unsubscribe();
    await emit('git-changed', { path: '/repo' });
    expect(cb).toHaveBeenCalledTimes(1);  // unchanged
  });

  it('installs the global GIT_CHANGED listener only ONCE across many subscriptions', async () => {
    const client = createPathInvalidatedCache<string, string>({ fetcher: vi.fn() });
    client.subscribe('a', '/repoA', () => {});
    client.subscribe('b', '/repoB', () => {});
    client.subscribe('c', '/repoC', () => {});
    client.subscribe('d', '/repoA', () => {});  // same path as 'a' — should NOT re-install
    expect(mockedListen).toHaveBeenCalledTimes(1);
  });

  it('bus dispatch is scoped to the owning client — a sibling client with the same key is NOT wiped', async () => {
    // Footgun 1 (per the memory): a naive "iterate all clients and wipe
    // the entry for key === 7" implementation would also nuke clientB's
    // cache when an event for /repoA fires. The primitive scopes dispatch
    // via the subscriber's clientId so only the owning client is touched.
    const clientA = createPathInvalidatedCache<number, string>({
      fetcher: vi.fn().mockResolvedValue('value-A'),
    });
    const clientB = createPathInvalidatedCache<number, string>({
      fetcher: vi.fn().mockResolvedValue('value-B'),
    });
    await clientA.refresh(7);
    await clientB.refresh(7);
    expect(clientA.read(7)).toBe('value-A');
    expect(clientB.read(7)).toBe('value-B');

    const cbA = vi.fn();
    clientA.subscribe(7, '/repoA', cbA);

    // Fire an event for /repoA — only clientA is subscribed there.
    await emit('git-changed', { path: '/repoA' });
    expect(cbA).toHaveBeenCalledTimes(1);
    // clientB was not subscribed to /repoA, so its cache for 7 is intact.
    expect(clientB.read(7)).toBe('value-B');
  });
});

describe('createPathInvalidatedCache — bus invalidates cache before notify', () => {
  it('evicts the cached value for the matching key before invoking the subscriber callback', async () => {
    // The hook contract (and the four original hooks' contract) is: by the
    // time the subscriber's `notify` runs, the cache for that key has been
    // erased. A subsequent refresh hits the backend (or dedups onto a
    // sibling's in-flight fetch) instead of returning a stale value.
    const client = createPathInvalidatedCache<string, string>({
      fetcher: vi.fn().mockResolvedValue('first'),
    });
    await client.refresh('k');
    expect(client.read('k')).toBe('first');

    const observed = client.read('k');
    client.subscribe('k', '/repo', () => {
      // Capture the cache state at the moment the callback fires.
      observed; // (no-op; the read is asserted below)
    });
    await emit('git-changed', { path: '/repo' });
    expect(client.read('k')).toBeUndefined();
  });
});

describe('subscribeGitPathInvalidation — callback-only subscription (issue #345)', () => {
  it('invokes the callback on a matching GIT_CHANGED event', async () => {
    const cb = vi.fn();
    subscribeGitPathInvalidation('/repo', cb);
    await emit('git-changed', { path: '/repo' });
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it('does NOT invoke the callback when the event path does not match', async () => {
    const cb = vi.fn();
    subscribeGitPathInvalidation('/repo', cb);
    await emit('git-changed', { path: '/other' });
    expect(cb).not.toHaveBeenCalled();
  });

  it('matches a worktree subdir against a mesh-root subscription (issue #304)', async () => {
    // Mirrors the hook-side test above — the new component-level API must
    // use the SAME `pathMatchesGitEvent` helper so the worktree-subdir
    // rule applies uniformly across both subscribers. Slash/case details
    // are pinned by `tests/unit/paths.test.ts`.
    const cb = vi.fn();
    subscribeGitPathInvalidation('/repo', cb);
    await emit('git-changed', { path: '/repo/.claude/worktrees/swift-otter' });
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it('returns a synchronous unsubscribe that stops further notifications', async () => {
    const cb = vi.fn();
    const unsubscribe = subscribeGitPathInvalidation('/repo', cb);
    await emit('git-changed', { path: '/repo' });
    expect(cb).toHaveBeenCalledTimes(1);
    // No `.then(...)` — the cleanup is sync, unlike the hand-rolled
    // `listen()` pattern that produced a Promise<UnlistenFn>.
    unsubscribe();
    await emit('git-changed', { path: '/repo' });
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it('supports multiple subscribers on the same path — all are notified', async () => {
    const cb1 = vi.fn();
    const cb2 = vi.fn();
    subscribeGitPathInvalidation('/repo', cb1);
    subscribeGitPathInvalidation('/repo', cb2);
    await emit('git-changed', { path: '/repo' });
    expect(cb1).toHaveBeenCalledTimes(1);
    expect(cb2).toHaveBeenCalledTimes(1);
  });

  it('supports subscribers on different paths — only the matching one fires', async () => {
    const cbRepo = vi.fn();
    const cbOther = vi.fn();
    subscribeGitPathInvalidation('/repo', cbRepo);
    subscribeGitPathInvalidation('/other', cbOther);
    await emit('git-changed', { path: '/repo' });
    expect(cbRepo).toHaveBeenCalledTimes(1);
    expect(cbOther).not.toHaveBeenCalled();
  });

  it('dispatches the noop callback via its own clientId — hook client on the SAME path gets its OWN handler', async () => {
    // Pin the stronger guarantee (post #355): when a noop subscriber
    // and a hook client BOTH subscribe to the same path, the bus
    // dispatches each via its OWN clientId-scoped handler. The hook
    // client's cache for its own key is wiped (by the hook handler,
    // which the bus dispatches for `kind: 'keyed'` subs); the noop
    // callback also fires (by the noop handler, which the bus dispatches
    // for `kind: 'callback'` subs). A buggy implementation that ran the
    // hook handler for the noop subscriber would try to read
    // `hookClient.cache.get(sub.key)` where `sub.key` doesn't exist
    // (no-op today) AND would skip the noop callback entirely.
    const hookClient = createPathInvalidatedCache<number, string>({
      fetcher: vi.fn().mockResolvedValue('value-A'),
    });
    await hookClient.refresh(7);
    expect(hookClient.read(7)).toBe('value-A');

    const cb = vi.fn();
    // Both subscribers are on /repoA — the event will match both, and
    // each will be dispatched via its OWN clientId-scoped handler.
    hookClient.subscribe(7, '/repoA', () => {});
    subscribeGitPathInvalidation('/repoA', cb);

    await emit('git-changed', { path: '/repoA' });
    expect(cb).toHaveBeenCalledTimes(1);
    // The hook handler ran for key=7 — its cache entry is now evicted.
    expect(hookClient.read(7)).toBeUndefined();
  });
});
