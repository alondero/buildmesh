import { describe, it, expect, vi } from 'vitest';
import { renderHook, act } from '@testing-library/react';
import { emit } from '@tauri-apps/api/event';
import { useGitPathInvalidation } from '../../src/hooks/useGitPathInvalidation';
import { GIT_CHANGED } from '../../src/lib/events';

// Force the helper to behave as on Windows so the case-insensitive +
// backslash normalisation branches run regardless of the host running
// the suite. Mirrors the other primitive-level tests.
vi.mock('../../src/lib/platform', () => ({
  isMac: false,
  isWindows: true,
}));

describe('useGitPathInvalidation (issue #357)', () => {
  it('invokes the callback on a matching GIT_CHANGED event for the watched path', async () => {
    const cb = vi.fn();
    renderHook(() => useGitPathInvalidation('/repo', cb));
    await act(async () => {
      await emit(GIT_CHANGED, { path: '/repo' });
    });
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it('does NOT invoke the callback for an event on a different path', async () => {
    const cb = vi.fn();
    renderHook(() => useGitPathInvalidation('/repo', cb));
    await act(async () => {
      await emit(GIT_CHANGED, { path: '/other' });
    });
    expect(cb).not.toHaveBeenCalled();
  });

  it('unsubscribes on unmount — no callbacks after the component is gone', async () => {
    const cb = vi.fn();
    const { unmount } = renderHook(() => useGitPathInvalidation('/repo', cb));
    await act(async () => {
      await emit(GIT_CHANGED, { path: '/repo' });
    });
    expect(cb).toHaveBeenCalledTimes(1);
    unmount();
    await act(async () => {
      await emit(GIT_CHANGED, { path: '/repo' });
    });
    expect(cb).toHaveBeenCalledTimes(1); // unchanged
  });

  it('drops the OLD-path callback when path changes mid-mount (race fix, issue #357)', async () => {
    // The hand-rolled `useEffect(() => subscribeGitPathInvalidation(path, cb))`
    // pattern has a race: when `path` changes, the cleanup unsubscribes
    // the old path and the new effect re-subscribes the new path. An
    // event that fires AFTER the cleanup but BEFORE the new subscription
    // resolves can re-invoke the *old* `cb` against the *new* component.
    // The hook flips a `cancelled` flag in the cleanup, so any in-flight
    // or queued callback invocation that races the path change is
    // short-circuited.
    //
    // Pin: after the path change, an emit on the OLD path must NOT
    // invoke the new cb. (The OLD path's subscription is unsubscribed
    // in the cleanup, so the listener is gone — this test pins the
    // listener teardown. The cancelled-flag protection is the second
    // line of defense for the gap between the cleanup's
    // `cancelled = true` and the listener's removal.)
    const cb = vi.fn();
    const { rerender, unmount } = renderHook(
      ({ path }: { path: string }) => useGitPathInvalidation(path, cb),
      { initialProps: { path: '/old' } }
    );

    await act(async () => {
      await emit(GIT_CHANGED, { path: '/old' });
    });
    expect(cb).toHaveBeenCalledTimes(1);

    // Switch paths — the cleanup unsubscribes /old, the next effect
    // subscribes /new.
    rerender({ path: '/new' });
    // Emit on the OLD path — the /old subscription is gone, so the
    // hook's listener is the only thing in the mock set for /old. With
    // the cleanup having unsubscribed, this emit must reach zero
    // subscribers and the cb must NOT be called.
    await act(async () => {
      await emit(GIT_CHANGED, { path: '/old' });
    });
    expect(cb).toHaveBeenCalledTimes(1); // unchanged

    // Emit on the NEW path — the new subscription fires.
    await act(async () => {
      await emit(GIT_CHANGED, { path: '/new' });
    });
    expect(cb).toHaveBeenCalledTimes(2);

    unmount();
  });

  it('drops the callback when the path becomes null/undefined (disables the subscription)', async () => {
    const cb = vi.fn();
    const { rerender } = renderHook(
      ({ path }: { path: string | null }) => useGitPathInvalidation(path, cb),
      { initialProps: { path: '/repo' as string | null } }
    );
    await act(async () => {
      await emit(GIT_CHANGED, { path: '/repo' });
    });
    expect(cb).toHaveBeenCalledTimes(1);

    rerender({ path: null });
    await act(async () => {
      await emit(GIT_CHANGED, { path: '/repo' });
    });
    expect(cb).toHaveBeenCalledTimes(1); // unchanged
  });

  it('does NOT re-subscribe when only `cb` changes (refs read the latest cb)', async () => {
    // Code-review finding: the hook previously listed `cb` in the deps
    // array, which tore down and rebuilt the bus subscription on every
    // render of consumers that pass inline arrows (`CenterDiffOverlay`).
    // The fix wraps `cb` in a ref and depends on
    // `[path]` only. Pin: changing the `cb` ref alone must NOT trigger
    // a re-subscribe — the subscription count is the observable signal.
    //
    // We can't observe the bus's internal subscriber set directly, so
    // we use a different observable: a side-effect-bearing `cb` whose
    // closure captures a counter that changes on each render. If the
    // hook re-subscribed on every cb change, the captured counter would
    // be the new value on the first emit after the rerender. With the
    // ref pattern, the LATEST cb runs on every emit, so the new
    // counter value is observed.
    let _counter = 0;
    const cb1 = vi.fn(() => {
      // The current closure captures `counter = 0` — if the hook called
      // this on the second emit, we'd see counter === 0 in the marker.
    });
    const cb2 = vi.fn(() => {
      // The current closure captures `counter = 1` — if the hook called
      // THIS on the second emit, we'd see counter === 1 in the marker.
    });
    const { rerender } = renderHook(
      ({ cb }: { cb: () => void }) =>
        useGitPathInvalidation('/repo', cb),
      { initialProps: { cb: cb1 } }
    );

    await act(async () => {
      await emit(GIT_CHANGED, { path: '/repo' });
    });
    expect(cb1).toHaveBeenCalledTimes(1);
    expect(cb2).not.toHaveBeenCalled();

    // Simulate a parent re-render that passes a fresh `cb` (e.g. an
    // inline arrow) WITHOUT changing `path`. The hook must NOT
    // re-subscribe — instead, it should call the LATEST cb on the
    // next event (via the ref).
    _counter = 1;
    rerender({ cb: cb2 });

    await act(async () => {
      await emit(GIT_CHANGED, { path: '/repo' });
    });
    // The ref-based design routes through cbRef.current(), so cb2 fires.
    // A deps-based design would have re-subscribed with cb1, so cb1
    // would fire instead and cb2 wouldn't.
    expect(cb2).toHaveBeenCalledTimes(1);
    expect(cb1).toHaveBeenCalledTimes(1); // unchanged — re-subscribe did NOT happen
  });
});

describe('useGitPathInvalidation extraPaths (issue #1519)', () => {
  it('invokes the callback for events under an extra dir outside the watched root', async () => {
    const cb = vi.fn();
    renderHook(() => useGitPathInvalidation('/repo/mesh', cb, { extraPaths: ['/tmp/wt'] }));
    await act(async () => {
      await emit(GIT_CHANGED, { path: '/tmp/wt/my-node' });
    });
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it('still matches the watched root and legacy worktree prefix with extras set', async () => {
    const cb = vi.fn();
    renderHook(() => useGitPathInvalidation('/repo/mesh', cb, { extraPaths: ['/tmp/wt'] }));
    await act(async () => {
      await emit(GIT_CHANGED, { path: '/repo/mesh/.claude/worktrees/n' });
    });
    expect(cb).toHaveBeenCalledTimes(1);
  });

  it('does NOT invoke for unrelated paths even with extras set', async () => {
    const cb = vi.fn();
    renderHook(() => useGitPathInvalidation('/repo/mesh', cb, { extraPaths: ['/tmp/wt'] }));
    await act(async () => {
      await emit(GIT_CHANGED, { path: '/elsewhere/x' });
    });
    expect(cb).not.toHaveBeenCalled();
  });
});
