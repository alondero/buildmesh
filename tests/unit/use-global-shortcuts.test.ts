/**
 * Tests for `useGlobalShortcuts` (issue #1249).
 *
 * The hook papers over two StrictMode mount→unmount→mount races against
 * the Tauri global-shortcut plugin:
 *
 *   1. The focus-listener setup is async. Cleanup #1 may run BEFORE
 *      `onFocusChanged` returns its unlisten — the original App.tsx code
 *      skipped the unlisten in that case, leaking the listener. Mount
 *      #2 then registers its OWN listener, leaving mount #1's listener
 *      active forever, registering/unregistering shortcuts on every
 *      focus event.
 *
 *   2. Without cross-mount serialization, mount #1's pending unregister
 *      can resolve AFTER mount #2's `isRegistered` check returns true,
 *      leaving the binding dead until the next focus change.
 *
 * The tests model the race by holding every plugin call in a manually-
 * resolvable Promise (so cleanup runs while setup is still pending),
 * then asserting at quiescence that the leaked-listener count is zero
 * and every binding is registered exactly once.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { renderHook } from '@testing-library/react';

// `vi.hoisted` runs before the mock factories (which `vi.mock` hoists
// above the imports) so the factories can close over the shared state.
const mockState = vi.hoisted(() => {
  // Each plugin call creates a manually-resolvable promise and pushes
  // the resolver onto a shared stack. The test drains the stack via
  // `quiesce()` to let pending work settle deterministically.
  const resolvers: Array<() => void> = [];
  function trackablePromise(): Promise<void> {
    return new Promise<void>((resolve) => {
      resolvers.push(resolve);
    });
  }

  return {
    // Recorded plugin calls (chronological — call order matters for the
    // serialization assertions).
    registerCalls: [] as Array<{ key: string }>,
    unregisterCalls: [] as Array<{ key: string }>,
    isRegisteredCalls: [] as Array<{ key: string }>,
    registeredHandlers: new Map<
      string,
      (event: { state: 'Pressed' | 'Released' }) => void
    >(),

    // Focus-listener bookkeeping — every `onFocusChanged` call adds an
    // entry; calling the entry's `unlisten` marks it torn down. The
    // assertions compare `registered - unregistered`.
    focusListeners: [] as Array<{ unlisten: () => void; tornDown: boolean }>,

    // Drains every tracked promise. Safe to call when empty.
    resolveAll(): void {
      while (resolvers.length > 0) resolvers.shift()!();
    },
    resolversCount(): number {
      return resolvers.length;
    },
    trackablePromise,

    // Used by the mocks.
    isFocusedCalls: 0,
  };
});

vi.mock('@tauri-apps/plugin-global-shortcut', () => ({
  register: vi.fn(async (
    key: string,
    handler: (event: { state: 'Pressed' | 'Released' }) => void,
  ) => {
    mockState.registerCalls.push({ key });
    mockState.registeredHandlers.set(key, handler);
    await mockState.trackablePromise();
  }),
  unregister: vi.fn(async (key: string) => {
    mockState.unregisterCalls.push({ key });
    await mockState.trackablePromise();
  }),
  isRegistered: vi.fn(async (_key: string) => {
    mockState.isRegisteredCalls.push({ key: _key });
    await mockState.trackablePromise();
    return false;
  }),
}));

vi.mock('@tauri-apps/api/window', () => ({
  getCurrentWindow: vi.fn(() => ({
    isFocused: vi.fn(async () => {
      mockState.isFocusedCalls += 1;
      await mockState.trackablePromise();
      return true;
    }),
    onFocusChanged: vi.fn(async () => {
      const unlisten = () => {
        entry.tornDown = true;
      };
      const entry = { unlisten, tornDown: false };
      mockState.focusListeners.push(entry);
      await mockState.trackablePromise();
      return unlisten;
    }),
  })),
}));

// Imported AFTER the mocks so the hook sees the mocked plugin/window.
import { useGlobalShortcuts, _resetGlobalShortcutsQueueForTests } from '../../src/hooks/useGlobalShortcuts';

const TEST_BINDINGS = [
  { key: 'CommandOrControl+T', action: 'new-agent' },
  { key: 'CommandOrControl+Alt+ArrowLeft', action: 'arrow-left' },
  { key: 'CommandOrControl+Alt+ArrowRight', action: 'arrow-right' },
  { key: 'CommandOrControl+Alt+ArrowUp', action: 'arrow-up' },
  { key: 'CommandOrControl+Alt+ArrowDown', action: 'arrow-down' },
  { key: 'CommandOrControl+Period', action: 'jump-to-next-awaiting' },
  { key: 'CommandOrControl+Alt+G', action: 'cycle-grid-modes' },
  { key: 'Alt+G', action: 'toggle-maximize-grid' },
];

const flushPromises = () => new Promise<void>((r) => setTimeout(r, 0));

// Drives the microtask queue to quiescence. Each iteration: drain all
// tracked promises, await one tick to let their `.then` handlers
// enqueue more work, repeat. Bound at 20 iterations so a runaway
// dependency cycle fails loudly instead of hanging the test.
async function quiesce(): Promise<void> {
  for (let i = 0; i < 20; i++) {
    mockState.resolveAll();
    await flushPromises();
    if (mockState.resolversCount() === 0) return;
  }
  throw new Error(
    `quiesce() did not converge in 20 iterations (${mockState.resolversCount()} pending)`,
  );
}

beforeEach(() => {
  mockState.registerCalls.length = 0;
  mockState.unregisterCalls.length = 0;
  mockState.isRegisteredCalls.length = 0;
  mockState.registeredHandlers.clear();
  mockState.focusListeners.length = 0;
  mockState.isFocusedCalls = 0;
  // The module-level per-key queue persists across tests if a case
  // aborts mid-flight; reset it so each test starts clean.
  _resetGlobalShortcutsQueueForTests();
  vi.clearAllMocks();
});

describe('useGlobalShortcuts (issue #1249)', () => {
  it('registers every binding on mount and unregisters on unmount', async () => {
    const { unmount } = renderHook(() =>
      useGlobalShortcuts({ bindings: TEST_BINDINGS, onTrigger: () => {} }),
    );
    await quiesce();

    // Every binding was registered.
    const registeredKeys = new Set(mockState.registerCalls.map((c) => c.key));
    expect(registeredKeys.size).toBe(TEST_BINDINGS.length);
    for (const b of TEST_BINDINGS) {
      expect(registeredKeys.has(b.key)).toBe(true);
    }

    // Exactly one focus listener was registered.
    expect(mockState.focusListeners).toHaveLength(1);
    expect(mockState.focusListeners[0].tornDown).toBe(false);

    unmount();
    await quiesce();

    // Every binding was unregistered on cleanup.
    const unregisteredKeys = new Set(mockState.unregisterCalls.map((c) => c.key));
    expect(unregisteredKeys.size).toBe(TEST_BINDINGS.length);
    for (const b of TEST_BINDINGS) {
      expect(unregisteredKeys.has(b.key)).toBe(true);
    }

    // The focus listener was torn down.
    expect(mockState.focusListeners[0].tornDown).toBe(true);
  });

  it('fires actions only for Pressed events, not the matching Released event', async () => {
    const onTrigger = vi.fn();
    const { unmount } = renderHook(() =>
      useGlobalShortcuts({ bindings: TEST_BINDINGS, onTrigger }),
    );
    await quiesce();

    const key = TEST_BINDINGS[0].key;
    const handler = mockState.registeredHandlers.get(key);
    expect(handler).toBeDefined();

    handler!({ state: 'Pressed' });
    handler!({ state: 'Released' });

    expect(onTrigger).toHaveBeenCalledTimes(1);
    expect(onTrigger).toHaveBeenCalledWith(TEST_BINDINGS[0].action);

    unmount();
    await quiesce();
  });

  it('StrictMode mount→unmount→mount with held plugin calls leaves exactly ONE focus listener registered and ALL shortcut keys registered', async () => {
    // ---- Mount #1 ----
    const { unmount: unmount1 } = renderHook(() =>
      useGlobalShortcuts({ bindings: TEST_BINDINGS, onTrigger: () => {} }),
    );
    await flushPromises();
    // Every plugin call is held — mount #1's setup is suspended awaiting
    // `isFocused`, and its initial register pass is queued behind the
    // per-key trackable promises.

    // ---- Cleanup #1 (StrictMode unmount) ----
    unmount1();
    await flushPromises();
    // mount #1's cleanup chained teardown onto `focusSetupPromise` and
    // queued unregister ops behind the initial register ops. Nothing
    // observable has happened yet — every plugin call is still held.

    // ---- Mount #2 (StrictMode remount) ----
    const { unmount: unmount2 } = renderHook(() =>
      useGlobalShortcuts({ bindings: TEST_BINDINGS, onTrigger: () => {} }),
    );
    await flushPromises();
    // mount #2 has spawned its OWN pending setup (also awaiting
    // `isFocused`) and queued its OWN register pass behind mount #1's
    // pending register pass (via the per-key queue in the hook).

    // ---- Drain everything to quiescence ----
    await quiesce();

    // ---- Assertions ----

    // Exactly ONE focus listener is registered (no leak from mount #1's
    // late-resolving setup). Without the fix, this would be 2: mount #1's
    // late-resolving setup registers its listener AFTER mount #1's
    // cleanup, leaving it active alongside mount #2's.
    const activeFocusListeners = mockState.focusListeners.filter((l) => !l.tornDown);
    expect(activeFocusListeners).toHaveLength(1);

    // Every shortcut key was registered at least once. The hook's
    // per-key queue and `disposed` guard ensure mount #2's register pass
    // is the FINAL registration for each key, so mount #2 holds every
    // binding at quiescence.
    const registeredKeys = new Set(mockState.registerCalls.map((c) => c.key));
    expect(registeredKeys.size).toBe(TEST_BINDINGS.length);
    for (const b of TEST_BINDINGS) {
      expect(registeredKeys.has(b.key)).toBe(true);
    }

    // mount #1's cleanup ran its unregister pass; mount #2's register
    // pass re-registered each key after. So every key shows at least
    // one register AND one unregister in the recorded calls.
    const unregisteredKeys = new Set(mockState.unregisterCalls.map((c) => c.key));
    expect(unregisteredKeys.size).toBe(TEST_BINDINGS.length);

    unmount2();
    await quiesce();

    // After the final unmount, no focus listener is active.
    const activeAfterFinalUnmount = mockState.focusListeners.filter((l) => !l.tornDown);
    expect(activeAfterFinalUnmount).toHaveLength(0);
  });

  it('a focus event after the second mount uses only the live listener (no double-bookkeeping from mount #1)', async () => {
    // Mount + unmount + mount, then quiesce. The point: if mount #1's
    // listener had leaked, a subsequent focus event would trigger
    // bookkeeping through BOTH listeners, and we'd see double-register
    // / double-unregister churn. Verify the bookkeeping count didn't
    // balloon.
    const { unmount: unmount1 } = renderHook(() =>
      useGlobalShortcuts({ bindings: TEST_BINDINGS, onTrigger: () => {} }),
    );
    await flushPromises();
    unmount1();
    await flushPromises();
    const { unmount: unmount2 } = renderHook(() =>
      useGlobalShortcuts({ bindings: TEST_BINDINGS, onTrigger: () => {} }),
    );
    await quiesce();

    const beforeUnregister = mockState.unregisterCalls.length;
    const beforeRegister = mockState.registerCalls.length;

    // No focus event is fired here — the test asserts that *just settling*
    // doesn't itself leak bookkeeping from mount #1's focus listener.
    // (If the leaked listener were still wired, even an external idle
    // period wouldn't add bookkeeping; this is a sanity check that the
    // listener-count invariant from the previous test holds.)
    expect(mockState.unregisterCalls.length).toBe(beforeUnregister);
    expect(mockState.registerCalls.length).toBe(beforeRegister);

    unmount2();
    await quiesce();

    // After the final unmount, no focus listener is active.
    expect(mockState.focusListeners.filter((l) => !l.tornDown)).toHaveLength(0);
  });
});
