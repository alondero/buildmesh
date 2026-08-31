import { useEffect } from 'react';
import {
  register,
  unregister,
  isRegistered,
  type ShortcutEvent,
} from '@tauri-apps/plugin-global-shortcut';
import { getCurrentWindow } from '@tauri-apps/api/window';

export type GlobalShortcutBinding = {
  key: string;
  action: string;
};

export type UseGlobalShortcutsOptions = {
  bindings: GlobalShortcutBinding[];
  /** Called when the OS dispatches one of the registered accelerators. */
  onTrigger: (action: string) => void;
};

// Module-level per-key serialization queue.
//
// Tauri's global-shortcut plugin state is process-wide — `register`,
// `unregister`, and `isRegistered` mutate shared state that outlives any
// single React effect. A StrictMode mount→unmount→mount cycle interleaves
// mount #1's pending unregister with mount #2's `isRegistered` check: if
// mount #2 sees "true" before mount #1's unregister resolves, it skips
// its own register, then mount #1's unregister resolves and strands the
// binding dead until the next focus change coincidentally re-registers
// it (issue #1249 failure mode #2).
//
// The queue is keyed by accelerator string and chains ops in submission
// order across all mounts. A late-arriving op on key K only starts after
// every prior op on K has resolved.
const pendingByKey = new Map<string, Promise<void>>();

function enqueueKeyOp(key: string, op: () => Promise<void>): Promise<void> {
  const prev = pendingByKey.get(key) ?? Promise.resolve();
  // Swallow op errors so a single failed register/unregister doesn't
  // stall subsequent operations on the same key. The op itself owns its
  // own error reporting (try/catch + console.warn).
  const next = prev.then(() => op()).catch(() => {});
  pendingByKey.set(key, next);
  // Tail-cleanup: drop the Map entry once this op is the head. Safe
  // even if a later enqueue wrote a NEW head over ours — the equality
  // check filters those out.
  void next.finally(() => {
    if (pendingByKey.get(key) === next) pendingByKey.delete(key);
  });
  return next;
}

/** @internal — exposed for tests so the queue starts empty per case. */
export function _resetGlobalShortcutsQueueForTests(): void {
  pendingByKey.clear();
}

/**
 * Wires a list of Tauri global shortcuts and keeps them synced with the
 * window's focused state.
 *
 * Two StrictMode races are papered over (issue #1249):
 *
 *   1. The focus-listener setup is async. Cleanup #1 may run BEFORE
 *      `onFocusChanged` returns its unlisten, so the original App.tsx
 *      code silently skipped tearing it down. Mount #2 then registered
 *      its OWN listener, leaving mount #1's listener alive forever and
 *      registering/unregistering shortcuts on every focus change.
 *
 *   2. register/unregister bookkeeping per accelerator can race across
 *      mounts: mount #2's `isRegistered` check can return a stale
 *      "true" while mount #1's pending unregister is still in flight.
 *      The module-level per-key promise queue above keeps mount #2's
 *      bookkeeping fully behind mount #1's cleanup.
 *
 * The hook is a drop-in replacement for the inline effect that used to
 * live in `src/App.tsx`. The `bindings` and `onTrigger` are captured on
 * mount; the consumer passes stable references so the `[]` deps stay
 * honest.
 */
export function useGlobalShortcuts({ bindings, onTrigger }: UseGlobalShortcutsOptions): void {
  useEffect(() => {
    // `disposed` flips on cleanup. The closure reference (not a value
    // snapshot) is what late-arriving async work consults — `disposed`
    // is captured by reference inside the focus handler and the queued
    // ops, so a remount that sets the SAME variable to `false` would
    // affect prior closures too. That's fine here because each effect
    // run declares its OWN `disposed`: a new mount has a new closure.
    let disposed = false;

    // Tracks the window's focused state at the time of a keypress.
    // The OS-level handler closure consults this ref before firing
    // `onTrigger` — defense-in-depth against the TOCTOU window between
    // `onFocusChanged` reporting focus-loss and `unregister` settling
    // (the binding would otherwise still receive the keystroke and
    // emit while the user is typing in another app).
    const isFocusedRef: { current: boolean } = { current: true };

    const triggerOnPress = (action: string) => (event: ShortcutEvent) => {
      // Tauri emits both ends of a global shortcut's lifecycle. These
      // actions are one-shot gestures, so release must not replay them
      // (a toggle would otherwise open on press and close on release).
      if (event.state !== 'Pressed') return;
      if (!isFocusedRef.current) return;
      onTrigger(action);
    };

    async function tryRegister(key: string, action: string): Promise<void> {
      await enqueueKeyOp(key, async () => {
        try {
          if (disposed) return;
          if (await isRegistered(key)) return;
          // Re-check after the await: `disposed` may have flipped while
          // `isRegistered` was in flight. The per-key queue still
          // preserves submission order; we just don't register against a
          // stale "not registered" snapshot.
          if (disposed) return;
          await register(key, triggerOnPress(action));
        } catch (e) {
          console.warn(`Failed to register shortcut ${key}:`, e);
        }
      });
    }

    async function tryUnregister(key: string): Promise<void> {
      await enqueueKeyOp(key, async () => {
        try {
          // Only unregister if THIS mount has actually been disposed.
          // Without this guard, a focus-driven unregister (still active
          // while we're disposing) would tear down the next mount's
          // registration.
          if (!disposed) return;
          await unregister(key);
        } catch (e) {
          console.warn(`Failed to unregister shortcut ${key}:`, e);
        }
      });
    }

    // Initial registration pass — fire and forget. We deliberately do
    // NOT await the queue so a synchronous cleanup (StrictMode's
    // unmount) can chain its teardown through the same queue and
    // settle in submission order with the initial register.
    for (const { key, action } of bindings) {
      void tryRegister(key, action);
    }

    // Focus tracking. The setup is async; we capture both the unlisten
    // function and the Promise that produces it so cleanup #1 can chain
    // the teardown onto a pending setup (issue #1249 failure mode #1).
    let focusUnlisten: (() => void) | null = null;
    const focusSetupPromise = (async () => {
      try {
        const win = getCurrentWindow();
        const focused = await win.isFocused();
        if (disposed) return;
        isFocusedRef.current = focused;
        focusUnlisten = await win.onFocusChanged(async ({ payload: focused }) => {
          // A late focus event after unmount must be a no-op. The
          // handler closure outlives this effect's local scope, so we
          // re-check `disposed` (captured by reference).
          if (disposed) return;
          isFocusedRef.current = focused;
          for (const { key, action } of bindings) {
            void enqueueKeyOp(key, async () => {
              try {
                if (focused) {
                  if (!(await isRegistered(key))) {
                    await register(key, triggerOnPress(action));
                  }
                } else {
                  await unregister(key);
                }
              } catch (e) {
                console.warn(`Failed to update shortcut ${key} on focus change:`, e);
              }
            });
          }
        });
      } catch (e) {
        console.warn('Failed to set up focus tracking:', e);
      }
    })();

    return () => {
      disposed = true;
      // Tear down the focus listener either NOW (if setup already
      // resolved and `focusUnlisten` is set) or BY CHAINING onto the
      // pending setup promise. The chain re-checks `focusUnlisten`
      // because setup may have raced the cleanup and resolved in
      // between.
      const teardownFocus = () => {
        if (focusUnlisten) {
          focusUnlisten();
          focusUnlisten = null;
        }
      };
      if (focusUnlisten) {
        teardownFocus();
      } else {
        focusSetupPromise.then(teardownFocus);
      }
      // Best-effort unregister of every binding. `tryUnregister` is a
      // no-op unless `disposed` is true (set above), and queues behind
      // any in-flight register from this mount via the module-level
      // per-key queue — so submission order across mounts is
      // preserved.
      for (const { key } of bindings) {
        void tryUnregister(key);
      }
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps -- bindings/onTrigger captured on mount; consumer passes stable refs.
  }, []);
}
