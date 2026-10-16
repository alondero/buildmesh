/**
 * `useGitSummary` outcome contract (issue #2328).
 *
 * The summary has always collapsed three states into `summary: null` — clean
 * (total === 0), never observed, and the fetch failed. `GridNodeHeader` is an
 * active consumer of that `null`, so the collapsing was user-visible. These
 * tests pin the three apart over the real seam (shared client + mocked
 * `invoke`), not a stubbed hook.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { emit } from '@tauri-apps/api/event';
import { useGitSummary } from '../../src/hooks/useGitSummary';
import type { GitSummary } from '../../src/lib/tauri';

const DIRTY: GitSummary = { total: 3, added: 1, modified: 1, deleted: 1 };
const CLEAN: GitSummary = { total: 0, added: 0, modified: 0, deleted: 0 };

/** See `use-changed-files.test.ts` — call it only after the first fetch settles. */
function skipRefetchWindow() {
  const realNow = Date.now();
  const spy = vi.spyOn(Date, 'now').mockReturnValue(realNow + 60_000);
  return () => spy.mockRestore();
}

describe('useGitSummary — unavailable vs clean vs stale (issue #2328)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it('marks a failed first fetch unavailable while keeping summary null', async () => {
    vi.mocked(invoke).mockRejectedValue(new Error('not a repository'));

    const { result } = renderHook(() => useGitSummary('/repo'));

    await waitFor(() => expect(result.current.error).toBeInstanceOf(Error));
    expect(result.current.summary).toBeNull();
    expect(result.current.unavailable).toBe(true);
    expect(result.current.stale).toBe(false);
  });

  it('reports an observed clean repo as clean, not as unavailable', async () => {
    vi.mocked(invoke).mockResolvedValue(CLEAN);

    const { result } = renderHook(() => useGitSummary('/repo'));

    // `error` is null from the very first render, so waiting on it proves
    // nothing. Wait for the fetch to settle instead, then read the outcome.
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.error).toBeNull();
    // Zero total still hides the panel, but it is now a *known* clean state.
    expect(result.current.summary).toBeNull();
    expect(result.current.unavailable).toBe(false);
    expect(result.current.stale).toBe(false);
  });

  it('keeps the counts and flags them stale when a background refresh fails', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(DIRTY);
    const { result } = renderHook(() => useGitSummary('/repo'));
    await waitFor(() => expect(result.current.summary).toEqual(DIRTY));

    const restoreClock = skipRefetchWindow();
    try {
      vi.mocked(invoke).mockRejectedValue(new Error('index.lock'));
      await act(async () => {
        await emit('git-changed', { path: '/repo' });
      });
      await waitFor(() => expect(result.current.error).toBeInstanceOf(Error));

      expect(result.current.summary).toEqual(DIRTY);
      expect(result.current.stale).toBe(true);
      expect(result.current.unavailable).toBe(false);
    } finally {
      restoreClock();
    }
  });

  it('reports a failed refresh after an observed clean repo as unavailable, not stale', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(CLEAN);
    const { result } = renderHook(() => useGitSummary('/repo'));
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.summary).toBeNull();
    expect(result.current.unavailable).toBe(false);
    expect(result.current.error).toBeNull();

    const restoreClock = skipRefetchWindow();
    try {
      vi.mocked(invoke).mockRejectedValue(new Error('index.lock'));
      await act(async () => {
        await emit('git-changed', { path: '/repo' });
      });
      await waitFor(() => expect(result.current.error).toBeInstanceOf(Error));
    } finally {
      restoreClock();
    }

    // Regression caught in review of #2389: the cache still holds
    // `{ total: 0 }`, so a `data`-derived `unavailable` stayed false while the
    // zero-total projection had already collapsed `summary` to `null`. A
    // compact surface that renders `summary` alone then showed nothing at all,
    // making a broken status indistinguishable from a healthy clean repo — the
    // exact confusion issue #2328 exists to remove.
    expect(result.current.summary).toBeNull();
    expect(result.current.unavailable).toBe(true);
    // There are no counts on screen, so nothing can be "last known".
    expect(result.current.stale).toBe(false);
  });

  it('recovers from a clean-repo failure when a retry succeeds', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(CLEAN);
    const { result } = renderHook(() => useGitSummary('/repo'));
    await waitFor(() => expect(result.current.loading).toBe(false));

    vi.mocked(invoke).mockRejectedValueOnce(new Error('index.lock'));
    await act(async () => {
      result.current.refresh();
    });
    await waitFor(() => expect(result.current.unavailable).toBe(true));

    vi.mocked(invoke).mockResolvedValueOnce(DIRTY);
    await act(async () => {
      result.current.refresh();
    });
    await waitFor(() => expect(result.current.summary).toEqual(DIRTY));
    expect(result.current.error).toBeNull();
    expect(result.current.unavailable).toBe(false);
    expect(result.current.stale).toBe(false);
  });

  it('clears the stale flag when a retry succeeds', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(DIRTY);
    const { result } = renderHook(() => useGitSummary('/repo'));
    await waitFor(() => expect(result.current.summary).toEqual(DIRTY));

    vi.mocked(invoke).mockRejectedValueOnce(new Error('index.lock'));
    await act(async () => {
      result.current.refresh();
    });
    await waitFor(() => expect(result.current.stale).toBe(true));

    const BIGGER: GitSummary = { total: 9, added: 5, modified: 4, deleted: 0 };
    vi.mocked(invoke).mockResolvedValueOnce(BIGGER);
    await act(async () => {
      result.current.refresh();
    });
    await waitFor(() => expect(result.current.summary).toEqual(BIGGER));
    expect(result.current.error).toBeNull();
    expect(result.current.stale).toBe(false);
  });

  it('does not carry one path\'s summary into the next path', async () => {
    vi.mocked(invoke).mockResolvedValueOnce(DIRTY);
    const { result, rerender } = renderHook(
      ({ path }: { path: string }) => useGitSummary(path),
      { initialProps: { path: '/first' } },
    );
    await waitFor(() => expect(result.current.summary).toEqual(DIRTY));

    vi.mocked(invoke).mockResolvedValueOnce(CLEAN);
    rerender({ path: '/second' });

    expect(result.current.summary).toBeNull();
    expect(result.current.stale).toBe(false);
    await waitFor(() => expect(result.current.error).toBeNull());
  });

  it('stays disabled and silent when no path is given', async () => {
    const { result } = renderHook(() => useGitSummary(null));

    await act(async () => {
      await emit('git-changed', { path: '/repo' });
    });
    expect(invoke).not.toHaveBeenCalled();
    expect(result.current.summary).toBeNull();
    expect(result.current.unavailable).toBe(false);
  });
});