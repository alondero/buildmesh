import { getCurrentWindow } from '@tauri-apps/api/window';

/**
 * The OS focus reader shared by the caption family and the animation gate.
 *
 * The window's focus state is an OS fact, not a UI fact, so it is read from
 * Tauri's `onFocusChanged` event — that is where an alt-tab, a taskbar click
 * or a dialog in another process show up. `isFocused()` answers the initial
 * query, because a window that is already unfocused when the page loads
 * receives no focus-change event until it is brought forward.
 *
 * The caption family and the animation gate read the focus fact through this
 * one implementation, so they answer "does the window have focus" the same
 * way: the caption buttons dim from it (ADR-0035), the macOS traffic lights
 * grey from it (ADR-0036), and the idle animation gate pauses the decorative
 * infinite animations from it (issue #2599). Each consumer asks the OS for
 * itself — this module centralises the reading, not the state — but the
 * reading is the same one, written once.
 *
 * Two other hooks subscribe to the same Tauri event for their own purposes —
 * the global shortcut dispatcher (it must not fire while the window is in the
 * background) and the path-invalidated query (it refreshes when the window
 * comes back) — and they keep their own subscription, for two reasons. Both
 * need the TRANSITION, and this module deliberately does not separate the
 * initial answer from a transition: it delivers both through one callback, so
 * a consumer of `watchWindowFocus` cannot tell "the window was already
 * unfocused" from "the window just lost focus". The path-invalidated query
 * would refetch on mount if the initial answer reached it. And the shortcut
 * dispatcher keeps the answer in a ref that its own OS-level handler checks,
 * so the TOCTOU window between the focus event and the shortcut's
 * registration stays closed.
 *
 * `onChange` is called with the initial state as soon as the query resolves
 * and again on every focus change. The consumer owns its own default for the
 * window between those two moments — the caption strip stays lit until it is
 * told otherwise, and the animation gate keeps the animations running until
 * it is told otherwise.
 *
 * The returned function tears the subscription down. It is safe to call before
 * the subscription has resolved: the pending listener is then stopped as soon
 * as it arrives, so it never outlives the consumer.
 */
export function watchWindowFocus(onChange: (focused: boolean) => void): () => void {
  let disposed = false;
  let unlisten: (() => void) | null = null;
  // A focus event carries a LATER state than the initial query, so once one
  // has been seen the query's answer is stale and must not overwrite it.
  let sawEvent = false;

  const setup = async () => {
    let win: ReturnType<typeof getCurrentWindow>;
    try {
      win = getCurrentWindow();
    } catch (e) {
      console.warn('Failed to reach the window for focus tracking:', e);
      return;
    }

    // The initial query and the subscription are independent of each other. A
    // window whose focus query fails can still report a focus change later,
    // and a consumer that needs the transition (the animation gate) must not
    // be switched off for the rest of the session because one query failed.
    // The consumer keeps its own default until it hears otherwise: the caption
    // strip stays lit, the animations keep running.
    //
    // The query runs inside its own async function so a SYNCHRONOUS throw
    // from it is caught here too, instead of escaping as an unhandled
    // rejection the way a bare `win.isFocused()` would.
    void (async () => {
      try {
        const initial = await win.isFocused();
        if (!disposed && !sawEvent) onChange(initial);
      } catch (e) {
        console.warn('Failed to query the window focus state:', e);
      }
    })();

    try {
      const stop = await win.onFocusChanged(({ payload }) => {
        sawEvent = true;
        // A late event after teardown must be a no-op; the handler outlives
        // this scope, so it re-checks `disposed`.
        if (!disposed) onChange(payload);
      });
      // Subscribing is awaited too, so teardown can land between the call and
      // its resolution — by which point the cleanup has already run with
      // nothing to tear down. Without this the listener would outlive the
      // consumer; React 19 StrictMode's mount → cleanup → mount makes that
      // interleaving routine rather than exotic.
      if (disposed) {
        stop();
        return;
      }
      unlisten = stop;
    } catch (e) {
      console.warn('Failed to subscribe to window focus changes:', e);
    }
  };

  void setup();

  return () => {
    disposed = true;
    if (unlisten) unlisten();
  };
}
