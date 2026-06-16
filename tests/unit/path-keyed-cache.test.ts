import { describe, it, expect, beforeEach, vi } from 'vitest';
import { listen, emit } from '@tauri-apps/api/event';
import {
  createPathKeyedCache,
  resetPathInvalidatedCacheForTests,
} from '../../src/lib/pathInvalidatedCache';

// `pathMatchesGitEvent` is Windows-aware (case + slash normalisation).
// Force `isWindows = true` so the worktree-subdir match branches run
// regardless of the host running the suite.
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

describe('createPathKeyedCache — cache + read', () => {
  it('refresh populates the cache so subsequent read returns the value', async () => {
    const client = createPathKeyedCache<string>({
      fetcher: vi.fn().mockResolvedValue('hello'),
    });
    expect(client.read('/repo')).toBeUndefined();
    await client.refresh('/repo');
    expect(client.read('/repo')).toBe('hello');
  });

  it('read returns undefined for an uncached key', () => {
    const client = createPathKeyedCache<string>({
      fetcher: vi.fn(),
    });
    expect(client.read('never-set')).toBeUndefined();
  });

  it('invalidate evicts the cached value and the next refresh calls fetcher again', async () => {
    const fetcher = vi.fn().mockResolvedValueOnce('first').mockResolvedValueOnce('second');
    const client = createPathKeyedCache<string>({ fetcher });
    await client.refresh('/repo');
    expect(client.read('/repo')).toBe('first');
    client.invalidate('/repo');
    expect(client.read('/repo')).toBeUndefined();
    await client.refresh('/repo');
    expect(client.read('/repo')).toBe('second');
    expect(fetcher).toHaveBeenCalledTimes(2);
  });

  it('dedupes concurrent refresh calls into a single fetcher invocation', async () => {
    const fetcher = vi.fn().mockResolvedValue('v');
    const client = createPathKeyedCache<string>({ fetcher });
    await Promise.all([
      client.refresh('/repo'),
      client.refresh('/repo'),
      client.refresh('/repo'),
    ]);
    expect(fetcher).toHaveBeenCalledTimes(1);
  });
});

describe('createPathKeyedCache — null-as-value', () => {
  it('treats a resolved null as a real cached value (not uncached)', async () => {
    const client = createPathKeyedCache<string | null>({
      fetcher: vi.fn().mockResolvedValue(null),
    });
    expect(client.read('/repo')).toBeUndefined();
    await client.refresh('/repo');
    // The cache uses a Symbol sentinel to disambiguate cached-null from
    // uncached; the read API surfaces that as `null`, not `undefined`.
    expect(client.read('/repo')).toBeNull();
  });
});

describe('createPathKeyedCache — error handling', () => {
  it('resolves refresh to null on fetcher rejection and leaves the cache empty', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const client = createPathKeyedCache<string>({
      fetcher: vi.fn().mockRejectedValue(new Error('boom')),
    });
    await expect(client.refresh('/repo')).resolves.toBeNull();
    expect(client.read('/repo')).toBeUndefined();
    expect(warn).toHaveBeenCalledTimes(1);
    warn.mockRestore();
  });

  it('uses the `name` option as the log tag in console.warn', async () => {
    const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
    const client = createPathKeyedCache<string>({
      fetcher: vi.fn().mockRejectedValue(new Error('boom')),
      name: 'useGitSummary',
    });
    await client.refresh('/repo');
    expect(warn).toHaveBeenCalledWith(
      'useGitSummary: fetch failed for key',
      '/repo',
      expect.any(Error),
    );
    warn.mockRestore();
  });
});

describe('createPathKeyedCache — bus + subscribe', () => {
  it('notifies subscribers on a matching GIT_CHANGED event (single-key, no path arg)', async () => {
    // The key IS the path: subscribe(key, cb) — the duplicated `path, path`
    // arg that the old single-factory API forced is gone.
    const client = createPathKeyedCache<string>({ fetcher: vi.fn() });
    const cb = vi.fn();
    client.subscribe('/repo', cb);
    await emit('git-changed', { path: '/repo' });
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it('matches a worktree subdir against a mesh-root subscription (issue #304)', async () => {
    // The subscriber registered with the mesh root `/repo`; the watcher
    // emits the worktree subdir. The primitive's listener uses
    // `pathMatchesGitEvent`. Detailed slash/case/normalisation coverage
    // lives in `tests/unit/paths.test.ts`; this is the wiring smoke test.
    const client = createPathKeyedCache<string>({ fetcher: vi.fn() });
    const cb = vi.fn();
    client.subscribe('/repo', cb);
    await emit('git-changed', { path: '/repo/.claude/worktrees/gentle-fox' });
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it('keeps different paths independent — both subscribers are notified', async () => {
    const client = createPathKeyedCache<string>({ fetcher: vi.fn() });
    const cbA = vi.fn();
    const cbB = vi.fn();
    client.subscribe('/repoA', cbA);
    client.subscribe('/repoB', cbB);
    await emit('git-changed', { path: '/repoA' });
    expect(cbA).toHaveBeenCalledTimes(1);
    expect(cbB).not.toHaveBeenCalled();
  });

  it('unsubscribe stops further notifications for that subscriber', async () => {
    const client = createPathKeyedCache<string>({ fetcher: vi.fn() });
    const cb = vi.fn();
    const unsubscribe = client.subscribe('/repo', cb);
    await emit('git-changed', { path: '/repo' });
    expect(cb).toHaveBeenCalledTimes(1);
    unsubscribe();
    await emit('git-changed', { path: '/repo' });
    expect(cb).toHaveBeenCalledTimes(1);  // unchanged
  });

  it('installs the global GIT_CHANGED listener only ONCE across many subscriptions', async () => {
    const client = createPathKeyedCache<string>({ fetcher: vi.fn() });
    client.subscribe('/repoA', () => {});
    client.subscribe('/repoB', () => {});
    client.subscribe('/repoC', () => {});
    client.subscribe('/repoA', () => {});  // same path as the first — should NOT re-install
    expect(mockedListen).toHaveBeenCalledTimes(1);
  });
});

describe('createPathKeyedCache — bus invalidates cache before notify', () => {
  it('evicts the cached value for the matching key before invoking the subscriber callback', async () => {
    const client = createPathKeyedCache<string>({
      fetcher: vi.fn().mockResolvedValue('first'),
    });
    await client.refresh('/repo');
    expect(client.read('/repo')).toBe('first');

    client.subscribe('/repo', () => {
      // The bus handler runs first, evicting the cache entry.
      // We assert this state at the next line.
    });
    await emit('git-changed', { path: '/repo' });
    expect(client.read('/repo')).toBeUndefined();
  });
});
