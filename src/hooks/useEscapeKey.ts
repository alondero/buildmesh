/**
 * useEscapeKey — last-mounted handler wins, single document-level dispatcher.
 *
 * Issue #649 — extracted from eight call sites that each attached their own
 * `window` / `document` keydown listener for Escape. When two surfaces were
 * mounted simultaneously (stacked modals, CommandOmnibar open while in Single
 * mode, DiffOverlayShell above a Single-view grid, a menu open inside the
 * circuit editor), every listener fired on a single Escape press and closed
 * all of them at once. The fix is two-part:
 *
 *  1. Factor the listener-attachment skeleton into a shared hook.
 *  2. Drive the dispatch from a **module-level LIFO stack** so the most
 *     recently mounted handler is the one that runs. React's mount order
 *     guarantees that child effects fire before parent effects, so an
 *     inner modal/overlay registered later sits on top of the stack and
 *     "shadows" any outer surface — the same conceptual rule as
 *     `AgentNodeView.tsx:402-405`'s guard list, but data-driven instead of
 *     an ad-hoc per-component predicate.
 *
 * Phase choice — bubble, not capture
 * -----------------------------------
 * The dispatcher uses `addEventListener('keydown', …, false)` (bubble
 * phase). Capture phase would fire before any element-level React
 * `onKeyDown`, silently bypassing the `e.stopPropagation()` opt-out in
 * `TitleBar/GridControls.tsx:105-111` (the search input there explicitly
 * stops the event so a modal/Single-mode Esc listener doesn't also fire).
 * Bubble phase keeps that pattern working: element-level handlers run
 * first, may call `stopPropagation` to opt out, and the dispatcher only
 * sees the event if they don't.
 *
 * IME composition carve-out
 * -------------------------
 * The dispatcher returns early when `e.isComposing` is true. This fixes a
 * latent bug inherited from `<Modal>`: pressing Escape to cancel a CJK
 * composition used to also close the modal. One carve-out at the
 * dispatcher level covers every consumer, instead of asking each call
 * site to remember.
 *
 * Lazy install (SSR-safe)
 * -----------------------
 * The document-level dispatcher is installed on the first hook mount, not
 * at module load. This mirrors `installTerminalZoomListener` in
 * `Terminal.tsx:56` and keeps the module safe to import in Node/SSR
 * environments where `document` is undefined.
 *
 * StrictMode safety
 * -----------------
 * Each stack entry has a stable id from a module-level counter, not an
 * array index. React 18 StrictMode's intentional double-invoke
 * (mount → unmount → mount) would otherwise desync array indices; the
 * id-based splice always removes the right entry.
 *
 * Test seam
 * ---------
 * `_resetEscapeKeyStackForTests` is wired into `tests/setup/vitest.setup.ts`
 * `beforeEach` next to `_resetGlobalShortcutsQueueForTests`, matching the
 * single-underscore convention used by `useGlobalShortcuts.ts:54`. RTL's
 * `cleanup()` already unmounts components between tests; the reset is
 * belt-and-suspenders for mid-mount errors, hook-level tests that don't
 * render via RTL, and StrictMode cycles within a single test.
 *
 * Out of scope
 * ------------
 * Modifier keys (`e.shiftKey`, `e.ctrlKey`) and auto-repeat (`e.repeat`)
 * are not interpreted at the dispatcher level — the handler receives the
 * event and can inspect them itself. `e.repeat` in particular: existing
 * surfaces don't check it, and matching their behaviour is the principle
 * of least surprise.
 *
 * Usage
 * -----
 *     useEscapeKey(() => setOpen(false), isOpen);
 *     useEscapeKey(handleEscape);  // arms for the component's lifetime
 */
import { useEffect, useRef } from 'react';

interface StackEntry {
  /** Stable per-entry id so StrictMode's double-invoke can safely splice. */
  id: number;
  /**
   * Closure that invokes the LATEST handler — captured via the hook's
   * `useRef` mirror, which is the same ref object across renders so the
   * closure always reads fresh state without re-attaching.
   */
  call: (event: KeyboardEvent) => void;
}

const stack: StackEntry[] = [];
let nextId = 1;
let dispatcherInstalled = false;

/**
 * Install the document-level keydown dispatcher once. Idempotent —
 * subsequent calls are no-ops. Guarded for Node/SSR environments where
 * `document` is undefined.
 */
function ensureDispatcher(): void {
  if (dispatcherInstalled) return;
  if (typeof document === 'undefined') return;
  dispatcherInstalled = true;
  // Bubble phase (third arg `false`) — see file-level comment on why
  // capture would regress `GridControls.tsx`'s `stopPropagation` opt-out.
  document.addEventListener('keydown', (event) => {
    if (event.key !== 'Escape') return;
    // IME composition carve-out — Escape during composition cancels the
    // composition, not the dialog.
    if (event.isComposing) return;
    // Top-of-stack wins. Iterate top-down so the most-recently-mounted
    // handler runs; if for some reason a dynamic predicate ever filters
    // the top entry, we'd fall through to the next. Today all entries
    // are always enabled, so we return after the first invocation.
    for (let i = stack.length - 1; i >= 0; i--) {
      const entry = stack[i];
      event.preventDefault();
      entry.call(event);
      return;
    }
  });
}

/**
 * Listen for Escape and invoke `handler` when this hook is the topmost
 * registered on the module-level stack.
 *
 * @param handler - Called with the `KeyboardEvent`. Receives the event
 *   so callers can inspect modifiers, etc.
 * @param enabled - When `false`, the hook is unregistered (no listener
 *   attached, no stack entry). Default `true`.
 */
export function useEscapeKey(
  handler: (event: KeyboardEvent) => void,
  enabled: boolean = true,
): void {
  // Mirror the handler through a ref so the long-lived stack entry
  // always invokes the LATEST closure (closure capture would freeze at
  // mount-time and miss later closures). Mirrors `useAriaMenu.ts:98-99`.
  const handlerRef = useRef(handler);
  handlerRef.current = handler;

  useEffect(() => {
    if (!enabled) return;
    ensureDispatcher();
    const id = nextId++;
    // The closure captures `handlerRef` (the same ref object across
    // renders, by `useRef` contract), so it always invokes the latest
    // handler even though the stack entry is created once per effect
    // run.
    stack.push({ id, call: (event) => handlerRef.current(event) });
    return () => {
      const idx = stack.findIndex((entry) => entry.id === id);
      if (idx >= 0) stack.splice(idx, 1);
    };
  }, [enabled]);
}

/**
 * Clear the module-level stack and reset the id counter. Wired into
 * `tests/setup/vitest.setup.ts` `beforeEach` so each test starts with
 * a fresh dispatcher. Safe to call from production code — there are no
 * live listeners after the manual `clear()` — but it's only exported
 * for the test seam.
 */
export function _resetEscapeKeyStackForTests(): void {
  stack.length = 0;
  nextId = 1;
}
