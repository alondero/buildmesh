/**
 * Tests for the `useMeshGitHubUrl` hook — backs the "View on GitHub"
 * context-menu item and the Issues / PRs probe header buttons.
 *
 * Pins the per-mesh cache contract:
 *   - the IPC is fired with the right `(meshId, ...)` arguments
 *   - a `null` return collapses to `url === null` (the consumer
 *     conditionally renders around it — the menu item hides, the
 *     probe header SafeLink falls back to a <span>)
 *   - a non-null string is surfaced as-is
 *
 * The hook is the seam between the React components and the
 * `get_github_url_for_mesh` IPC, so tests target the hook's return
 * shape (which is what the components consume) rather than the IPC
 * call directly. The IPC args are still asserted on so a future
 * refactor that adds a new arg or renames the existing one is caught.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { useMeshGitHubUrl } from '../../src/hooks/useMeshGitHubUrl';
import { GIT_CHANGED } from '../../src/lib/events';
import { listen } from '@tauri-apps/api/event';

vi.mocked(listen);

beforeEach(() => {
  vi.mocked(invoke).mockReset();
});

describe('useMeshGitHubUrl', () => {
  it('returns null when the mesh has no GitHub origin', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_github_url_for_mesh') return Promise.resolve(null);
      return Promise.resolve({});
    });
    const { result } = renderHook(() =>
      useMeshGitHubUrl(42, '/repos/demo'),
    );

    // Pin only the user-visible contract: `url` resolves to `null`.
    // (`loading` is part of the hook's return shape for parity with
    // `useOpenPr`, but no caller reads it; the `waitFor` on `url` is
    // the canonical "the IPC has settled" signal.)
    await waitFor(() => {
      expect(result.current.url).toBeNull();
    });
  });

  it('returns the resolved GitHub URL on success', async () => {
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_github_url_for_mesh') {
        return Promise.resolve('https://github.com/acme/demo');
      }
      return Promise.resolve({});
    });
    const { result } = renderHook(() =>
      useMeshGitHubUrl(42, '/repos/demo'),
    );

    await waitFor(() => {
      expect(result.current.url).toBe('https://github.com/acme/demo');
    });
    expect(result.current.loading).toBe(false);
    // Pin the IPC arg shape so a future refactor that drops the
    // meshId or renames the key is caught here rather than in a
    // downstream test that has to re-mock the response.
    expect(invoke).toHaveBeenCalledWith('get_github_url_for_mesh', { meshId: 42 });
  });

  it('returns null and skips the IPC when meshId is null', async () => {
    // `null` meshId is the "no mesh is active" case from
    // `useProbeContext()` (e.g. during the dock's "no project
    // selected" empty state). The hook should short-circuit
    // instead of firing a pointless IPC with a `null` meshId.
    const { result } = renderHook(() => useMeshGitHubUrl(null, null));

    // Wait one tick to ensure the effect has had a chance to run
    // before asserting the IPC was never called.
    await new Promise((r) => setTimeout(r, 0));
    expect(result.current.url).toBeNull();
    expect(result.current.loading).toBe(false);
    expect(invoke).not.toHaveBeenCalled();
  });

  it('refetches on GIT_CHANGED for the subscribed path', async () => {
    // First call: non-GitHub. After a GIT_CHANGED event for the
    // path, the cache invalidates and a refetch happens — this
    // time returning a real URL. The hook's
    // `subscribeByPath(key, path, cb)` contract is what we exercise.
    let invocationCount = 0;
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_github_url_for_mesh') {
        invocationCount += 1;
        // First call returns null; second (refetched) call returns a URL.
        return invocationCount === 1
          ? Promise.resolve(null)
          : Promise.resolve('https://github.com/acme/demo');
      }
      return Promise.resolve({});
    });

    // Capture the GIT_CHANGED listener the hook installs, so the
    // test can fire a synthetic event to trigger the refetch. The
    // pathInvalidatedCache primitive listens on the module-scoped
    // `listen` from `@tauri-apps/api/event`; we mock it and grab
    // the handler from the first call.
    const handlerRef: { current: ((e: { payload: unknown }) => void) | null } = { current: null };
    vi.mocked(listen).mockImplementation(async (event: string, handler: unknown) => {
      if (event === GIT_CHANGED) {
        handlerRef.current = handler as (e: { payload: unknown }) => void;
      }
      return () => {};
    });

    const { result } = renderHook(() => useMeshGitHubUrl(42, '/repos/demo'));

    await waitFor(() => {
      expect(invocationCount).toBeGreaterThanOrEqual(1);
    });
    expect(result.current.url).toBeNull();

    // Fire a GIT_CHANGED that matches the path. The bus primitive
    // does a path match — pass the path as the event payload so
    // `pathMatchesGitEvent` (tested separately in the primitive's
    // own tests) returns true.
    //
    // Issue #1386: wrap in `act` so React's state updates from the
    // listener are flushed before the `waitFor` reads `result.current` —
    // without it React prints "An update to TestComponent inside a test
    // was not wrapped in act(...)" for every refetch.
    await act(async () => {
      handlerRef.current?.({ payload: { path: '/repos/demo' } });
    });

    await waitFor(() => {
      expect(result.current.url).toBe('https://github.com/acme/demo');
    });
    expect(invocationCount).toBe(2);
  });
});
