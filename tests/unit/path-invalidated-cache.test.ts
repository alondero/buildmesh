import { describe, it, expect, beforeEach, vi } from 'vitest';
import { emit } from '@tauri-apps/api/event';
import {
  createPathKeyedCache,
  createDualKeyCache,
  resetPathInvalidatedCacheForTests,
  subscribeGitPathInvalidation,
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
});

describe('subscribeGitPathInvalidation — bus dispatches each subscriber via its own clientId', () => {
  it('a path-keyed cache client on the same path gets its OWN handler (no cross-client leakage)', async () => {
    // Pin the stronger guarantee (post #355): when a noop subscriber
    // and a cache client BOTH subscribe to the same path, the bus
    // dispatches each via its OWN clientId-scoped handler. The cache
    // client's cache for its own key is wiped (by the keyed handler,
    // which the bus dispatches for `kind: 'keyed'` subs); the noop
    // callback also fires (by the noop handler, which the bus
    // dispatches for `kind: 'callback'` subs). A buggy implementation
    // that ran the keyed handler for the noop subscriber would try to
    // read `cache.get(sub.key)` where `sub.key` doesn't exist (no-op
    // today) AND would skip the noop callback entirely.
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
    // The keyed handler ran — the pathClient's cache entry is now evicted.
    expect(pathClient.read('/repoA')).toBeUndefined();
  });

  it('a dual-key cache client on the same path gets its OWN handler (no cross-client leakage)', async () => {
    // Mirror of the above, with the dual-key shape: the keyed handler
    // evicts the dual-key client's cache for its own key, AND the
    // noop callback fires via the shared noop handler. Both
    // dispatches are scoped via `sub.clientId`.
    const dualClient = createDualKeyCache<number, string>({
      fetcher: vi.fn().mockResolvedValue('value-A'),
    });
    await dualClient.refresh(7);
    expect(dualClient.read(7)).toBe('value-A');

    const cb = vi.fn();
    dualClient.subscribeByPath(7, '/repoA', () => {});
    subscribeGitPathInvalidation('/repoA', cb);

    await emit('git-changed', { path: '/repoA' });
    expect(cb).toHaveBeenCalledTimes(1);
    expect(dualClient.read(7)).toBeUndefined();
  });
});
