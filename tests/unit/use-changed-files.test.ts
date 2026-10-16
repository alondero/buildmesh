/**
 * `useChangedFiles` outcome contract (issue #2328).
 *
 * These drive the real seam — the shared `gitStatusClient` over a mocked
 * `invoke` — rather than a stubbed hook, because the thing under test is
 * precisely how a rejected fetch is reported. The pre-#2328 hook returned
 * `files: []` for every failure, which is the bug: a consumer could not tell
 * "git says clean" from "git never answered".
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { useChangedFiles } from '../../src/hooks/useChangedFiles';
import type { GitStatus } from '../../src/lib/tauri';

const FILES: GitStatus[] = [
  { path: 'src/app.ts', status: 'modified', additions: 3, deletions: 1 },
];

const OTHER_FILES: GitStatus[] = [
  { path: 'lib/other.ts', status: 'added', additions: 9, deletions: 0 },
];

/**
 * The shared client caps GIT_CHANGED-driven refetches at one per 2s and arms
 * a trailing timer inside that window. Jump the clock past the window so the
 * emit refetches immediately — otherwise the assertion below would pass
 * vacuously (nothing refetched, so no error, so no stale label to find).
 *
 * Call this only AFTER the initial fetch settles: it has to advance time
 * *relative to* the stamp the first successful refresh left behind, and
 * installing it earlier would just stamp both reads with the same fake clock.
 */
function skipRefetchWindow() {
  const realNow = Date.now();
  const spy = vi.spyOn(Date, 'now').mockReturnValue(realNow + 60_000);
  return () => spy.mockRestore();
}

describe('useChangedFiles — unavailable vs clean vs stale (issue #2328)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it('reports an unavailable status rather than an empty list when the fetch fails', async () => {
    vi.mocked(invoke).mockRejectedValue(new Error('git exploded'));

    const { result } = renderHook(() => useChangedFiles('/repo'));

    await waitFor(() => expect(result.current.error).toBeInstanceOf(Error));
    // Before #2328 this was `[]`, indistinguishable from a clean repo.
    expect(result.current.files).toBeNull();
    expect(result.current.stale).toBe(false);
    expect(result.current.loading).toBe(false);
  });

  it('reports a clean repository as an observed empty list', async () => {
    vi.mocked(invoke).mockResolvedValue([]);

    const { result } = renderHook(() => useChangedFiles('/repo'));

    await waitFor(() => expect(result.current.files).toEqual([]));
    expect(result.current.error).toBeNull();
    expect(result.current.stale).toBe(false);
  });

  it('keeps the last known rows and flags them stale when a background refresh fails', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(FILES);
    const { result } = renderHook(() => useChangedFiles('/repo'));
    await waitFor(() => expect(result.current.files).toEqual(FILES));

    const restoreClock = skipRefetchWindow();
    try {
      vi.mocked(invoke).mockRejectedValue(new Error('index.lock'));
      await act(async () => {
        await emit('git-changed', { path: '/repo' });
      });
      await waitFor(() => expect(result.current.error).toBeInstanceOf(Error));

      // The rows are real and still worth showing — as last known.
      expect(result.current.files).toEqual(FILES);
      expect(result.current.stale).toBe(true);
    } finally {
      restoreClock();
    }
  });

  it('clears the error and replaces the stale rows when a retry succeeds', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(FILES);
    const { result } = renderHook(() => useChangedFiles('/repo'));
    await waitFor(() => expect(result.current.files).toEqual(FILES));

    vi.mocked(invoke).mockRejectedValueOnce(new Error('index.lock'));
    await act(async () => {
      result.current.refresh();
    });
    await waitFor(() => expect(result.current.stale).toBe(true));

    vi.mocked(invoke).mockResolvedValueOnce(OTHER_FILES);
    await act(async () => {
      result.current.refresh();
    });
    await waitFor(() => expect(result.current.files).toEqual(OTHER_FILES));
    expect(result.current.error).toBeNull();
    expect(result.current.stale).toBe(false);
  });

  it('cannot show the previous repository\'s rows while a new path loads', async () => {
    let resolveSecond: ((value: GitStatus[]) => void) | null = null;
    vi.mocked(invoke).mockImplementation((cmd: string, args: { path: string }) => {
      if (args.path === '/first') return Promise.resolve(FILES);
      return new Promise<GitStatus[]>((resolve) => {
        resolveSecond = resolve;
      });
    });

    const { result, rerender } = renderHook(
      ({ path }: { path: string }) => useChangedFiles(path),
      { initialProps: { path: '/first' } },
    );
    await waitFor(() => expect(result.current.files).toEqual(FILES));

    rerender({ path: '/second' });
    expect(result.current.files).toBeNull();

    await act(async () => {
      resolveSecond?.(OTHER_FILES);
    });
    await waitFor(() => expect(result.current.files).toEqual(OTHER_FILES));
  });

  it('keeps a failed path\'s error off the path that follows', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string, args: { path: string }) => {
      if (args.path === '/broken') return Promise.reject(new Error('broken repo'));
      return Promise.resolve(OTHER_FILES);
    });

    const { result, rerender } = renderHook(
      ({ path }: { path: string }) => useChangedFiles(path),
      { initialProps: { path: '/broken' } },
    );
    await waitFor(() => expect(result.current.error).toBeInstanceOf(Error));

    rerender({ path: '/healthy' });
    await waitFor(() => expect(result.current.files).toEqual(OTHER_FILES));
    expect(result.current.error).toBeNull();
  });

  it('stays disabled and silent when no path is given', async () => {
    const { result } = renderHook(() => useChangedFiles(null));

    await act(async () => {
      await emit('git-changed', { path: '/repo' });
    });
    expect(invoke).not.toHaveBeenCalled();
    expect(result.current.files).toBeNull();
    expect(result.current.error).toBeNull();
  });
});