import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
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

// Issue #342: `refresh()` collapses rejection to `null`, so consumers
// can't tell "fetch failed" from "legitimate null". The primitive now
// exposes the most-recent error per key via `lastError(key)` so callers
// (e.g. usePathInvalidatedQuery) can surface it in their result type.
// Both `createPathKeyedCache` and `createDualKeyCache` carry the new
// `lastError` accessor (it's added in the shared `createInternalClient`).
describe('lastError(key) — issue #342', () => {
  it('createPathKeyedCache: exposes the most recent error via lastError(key) after a rejection', async () => {
    const boom = new Error('boom');
    const client = createPathKeyedCache<string>({
      fetcher: vi.fn().mockRejectedValue(boom),
    });
    expect(client.lastError('k')).toBeNull();
    await client.refresh('k');
    expect(client.lastError('k')).toBe(boom);
  });

  it('createPathKeyedCache: clears lastError(key) on a subsequent successful refresh', async () => {
    const fetcher = vi
      .fn()
      .mockRejectedValueOnce(new Error('boom'))
      .mockResolvedValueOnce('ok');
    const client = createPathKeyedCache<string>({ fetcher });
    await client.refresh('k');
    expect(client.lastError('k')).toBeInstanceOf(Error);
    await client.refresh('k');
    expect(client.lastError('k')).toBeNull();
  });

  it('createPathKeyedCache: lastError is per-key — a failure for one key does not leak to another', async () => {
    const fetcher = vi.fn().mockImplementation((key: string) => {
      if (key === 'a') return Promise.reject(new Error('a-fail'));
      return Promise.resolve('b-value');
    });
    const client = createPathKeyedCache<string>({ fetcher });
    await client.refresh('a');
    await client.refresh('b');
    expect(client.lastError('a')).toBeInstanceOf(Error);
    expect(client.lastError('b')).toBeNull();
  });

  it('createDualKeyCache: exposes lastError(key) with the same per-key semantics', async () => {
    // Pin the dual-key shape's `lastError` works the same way — the
    // error slot is keyed on the entity id (the `key` arg), not on the
    // GIT_CHANGED path. Issue #342's contract applies to BOTH factories.
    const fetcher = vi
      .fn()
      .mockRejectedValueOnce(new Error('node-7-fail'))
      .mockResolvedValueOnce('node-8-value');
    const client = createDualKeyCache<number, string>({ fetcher });
    expect(client.lastError(7)).toBeNull();
    expect(client.lastError(8)).toBeNull();
    await client.refresh(7);
    expect(client.lastError(7)).toBeInstanceOf(Error);
    await client.refresh(8);
    expect(client.lastError(7)).toBeInstanceOf(Error);  // still set for 7
    expect(client.lastError(8)).toBeNull();
  });
});

// Issue #1165: `subscribeGitPathInvalidation` opts into the keyed cache's
// freshness window via `{ minRefetchIntervalMs }`. While the last fire for
// this subscriber is younger than the window, matching `GIT_CHANGED` events
// are suppressed and ONE trailing fire is armed at the window's expiry —
// a burst of agent edits collapses to one trailing refetch instead of one
// per emit. The freshness stamp is per-subscriber (each subscriber tracks
// its own "last invoked" timestamp) because callback subscribers have no
// cache value to compare against.
describe('subscribeGitPathInvalidation — minRefetchIntervalMs (issue #1165)', () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    vi.useRealTimers();
  });

  it('omitting options preserves the original behaviour — every matching event fires', async () => {
    const cb = vi.fn();
    subscribeGitPathInvalidation('/repo', cb);
    for (let i = 0; i < 5; i++) {
      vi.advanceTimersByTime(100);
      await emit('git-changed', { path: '/repo' });
    }
    expect(cb).toHaveBeenCalledTimes(5);
  });

  it('passing minRefetchIntervalMs: 0 is equivalent to omitting it', async () => {
    const cb = vi.fn();
    subscribeGitPathInvalidation('/repo', cb, { minRefetchIntervalMs: 0 });
    for (let i = 0; i < 5; i++) {
      vi.advanceTimersByTime(100);
      await emit('git-changed', { path: '/repo' });
    }
    expect(cb).toHaveBeenCalledTimes(5);
  });

  it('a burst of 5 events within 500 ms with a 2 s window fires the callback twice (immediate + one trailing)', async () => {
    // Pin the trailing-refetch contract. The backend coalescer emits up
    // to ~2/s during an agent edit burst; with the 2 s window the panel
    // collapses the burst to one immediate + one trailing refetch
    // instead of one per emit. Without this throttle, every emit fires
    // `diff_node_against_base` (`commands/diff.rs:602`), which
    // `run_blocking`-walks the worktree via libgit2 + 3× syntect per
    // hunk — the #761/#762 starvation class the issue calls out.
    const cb = vi.fn();
    subscribeGitPathInvalidation('/repo', cb, { minRefetchIntervalMs: 2_000 });

    // Five events spread across 500 ms — all inside the 2 s window after
    // the first one fires.
    for (let i = 0; i < 5; i++) {
      vi.advanceTimersByTime(100);
      await emit('git-changed', { path: '/repo' });
    }

    // The first event fires immediately (lastInvokedAt starts at 0, so the
    // freshness check passes on the very first emit). The next four are
    // suppressed, arming ONE trailing timer.
    expect(cb).toHaveBeenCalledTimes(1);

    // Advancing past the window fires the trailing — once, not four times.
    vi.advanceTimersByTime(1_600); // total elapsed ≈ 2_100 ms since first fire
    expect(cb).toHaveBeenCalledTimes(2);

    // No more trailing fires — the burst was suppressed into a single one.
    vi.advanceTimersByTime(5_000);
    expect(cb).toHaveBeenCalledTimes(2);
  });

  it('a single event with a 2 s window fires once — no trailing emit on a burst-of-one', async () => {
    // Pin the burst-of-one path: a single event inside an otherwise
    // idle window must not arm a trailing timer. Otherwise the
    // trailing would fire 2 s after every unrelated refresh, doubling
    // fetch rate for no reason.
    const cb = vi.fn();
    subscribeGitPathInvalidation('/repo', cb, { minRefetchIntervalMs: 2_000 });

    await emit('git-changed', { path: '/repo' });
    expect(cb).toHaveBeenCalledTimes(1);

    vi.advanceTimersByTime(3_000);
    expect(cb).toHaveBeenCalledTimes(1); // still one — no trailing fired
  });

  it('a trailing fire starts a fresh window — a second burst starts a new cycle', async () => {
    // After the trailing fires, `lastInvokedAt` is updated to the
    // trailing-fire timestamp, so the next event starts a fresh
    // window. Pin the cycle: burst → trailing fires → wait past the
    // window → second burst → second trailing.
    //
    // Use a 30 s window (matching the keyed-cache tests) so the timing
    // math is unambiguous. With smaller windows the off-by-100 between
    // the loop's `advance(N)` calls and the trailing's
    // `lastInvokedAt + window` fire time is easy to misread.
    const cb = vi.fn();
    subscribeGitPathInvalidation('/repo', cb, { minRefetchIntervalMs: 30_000 });

    // First burst — 5 events spread over 5 s, all inside the 30 s window.
    // The first emit fires immediately; the rest are suppressed and arm
    // a single trailing at the first suppressed emit's time.
    for (let i = 0; i < 5; i++) {
      vi.advanceTimersByTime(1_000);
      await emit('git-changed', { path: '/repo' });
    }
    expect(cb).toHaveBeenCalledTimes(1);

    // Advance past the window — the trailing fires once, not five times.
    vi.advanceTimersByTime(26_000);
    expect(cb).toHaveBeenCalledTimes(2);

    // Wait WELL past the new window before firing again — otherwise the
    // new burst falls inside the trailing-stamped window and is
    // suppressed (trailing fires update `lastInvokedAt` too, so the
    // window restarts). 35 s is comfortably past the 30 s window.
    vi.advanceTimersByTime(35_000);

    // Second burst — first event is past the trailing's window, fires
    // immediately; the rest are suppressed, arming a fresh trailing.
    for (let i = 0; i < 5; i++) {
      vi.advanceTimersByTime(1_000);
      await emit('git-changed', { path: '/repo' });
    }
    expect(cb).toHaveBeenCalledTimes(3);

    // Advance past the new window — second trailing fires.
    vi.advanceTimersByTime(26_000);
    expect(cb).toHaveBeenCalledTimes(4);
  });

  it('a re-arming burst does NOT extend the trailing timer (no runaway delay)', async () => {
    // Pin: once the trailing is armed, subsequent suppressed events
    // must NOT reset the timer. Resetting would push the trailing
    // further out for a continuously-editing agent — the settled
    // state would never land while edits stream in. The keyed branch
    // uses the same `if (!trailingTimers.has(key))` guard.
    //
    // Use a 30 s window so the 20 suppressed events (100 ms apart,
    // total 2 s) are unambiguously inside it. A 2 s window would
    // cross the boundary (`sinceLast < window` is strict) and trigger
    // a second immediate fire mid-burst, hiding the property under
    // test.
    const cb = vi.fn();
    subscribeGitPathInvalidation('/repo', cb, { minRefetchIntervalMs: 30_000 });

    await emit('git-changed', { path: '/repo' }); // fires immediately

    // A continuous stream of suppressed events — 20 of them at 100 ms
    // apart, all strictly inside the 30 s window.
    for (let i = 0; i < 20; i++) {
      vi.advanceTimersByTime(100);
      await emit('git-changed', { path: '/repo' });
    }
    expect(cb).toHaveBeenCalledTimes(1);

    // The trailing was armed after the first suppressed event at
    // t≈100 ms with delay ≈ 29_900 ms → fires at t≈30_000 ms total.
    // The subsequent 19 events must not have pushed it further out.
    vi.advanceTimersByTime(30_000); // total elapsed ≈ 32_100 ms since subscribe
    expect(cb).toHaveBeenCalledTimes(2);
  });

  it('freshness is per-subscriber — one subscriber\'s window does not affect another', async () => {
    // Pin the per-subscriber (not global) state: two callback-only
    // subscribers on the same path can each carry their own window
    // without interfering. This is the structural difference vs the
    // keyed branch, where the window is per-key (multiple subscribers
    // on the same key share the timer).
    const cbFast = vi.fn();
    const cbSlow = vi.fn();
    subscribeGitPathInvalidation('/repo', cbFast, { minRefetchIntervalMs: 500 });
    subscribeGitPathInvalidation('/repo', cbSlow, { minRefetchIntervalMs: 5_000 });

    await emit('git-changed', { path: '/repo' });
    expect(cbFast).toHaveBeenCalledTimes(1);
    expect(cbSlow).toHaveBeenCalledTimes(1);

    // 400 ms later — inside the 500 ms window for cbFast, well inside
    // the 5 s window for cbSlow. Both suppress.
    vi.advanceTimersByTime(400);
    await emit('git-changed', { path: '/repo' });
    expect(cbFast).toHaveBeenCalledTimes(1);
    expect(cbSlow).toHaveBeenCalledTimes(1);

    // cbFast's trailing fires at 500 ms (armed with delay ≈ 100 ms).
    vi.advanceTimersByTime(150); // total ≈ 550 ms since first fire
    expect(cbFast).toHaveBeenCalledTimes(2);
    expect(cbSlow).toHaveBeenCalledTimes(1); // still inside its 5 s window

    // cbSlow's trailing fires at the 5 s mark.
    vi.advanceTimersByTime(4_500);
    expect(cbSlow).toHaveBeenCalledTimes(2);
  });

  it('unsubscribe cancels any pending trailing timer — no fire after unmount', async () => {
    // Critical correctness property for the React glue: a component
    // that unmounts mid-burst must not see a stray `cb()` fire after
    // it's gone. The unsubscribe wrapper added in #1165 clears the
    // trailing timer deterministically.
    const cb = vi.fn();
    const unsubscribe = subscribeGitPathInvalidation('/repo', cb, {
      minRefetchIntervalMs: 2_000,
    });

    await emit('git-changed', { path: '/repo' }); // immediate fire
    vi.advanceTimersByTime(100);
    await emit('git-changed', { path: '/repo' }); // suppressed → trailing armed
    expect(cb).toHaveBeenCalledTimes(1);

    // Unmount mid-burst.
    unsubscribe();

    // Advancing past the window must NOT fire the cancelled timer.
    vi.advanceTimersByTime(10_000);
    expect(cb).toHaveBeenCalledTimes(1); // unchanged
  });
});
