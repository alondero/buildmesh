import { useEffect, useState } from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';

/** Whether the app window currently holds focus.
 *
 *  Drives the caption buttons' *inactive* state — one of Microsoft's five
 *  caption-button states (rest, hover, pressed, active, inactive) and the cue
 *  that tells you at a glance which window is taking your keystrokes. Read
 *  through `onFocusChanged` rather than tracked from local interaction, so an
 *  OS-level change (alt-tab, a taskbar click, a dialog in another process)
 *  lands here too.
 *
 *  `isFocused` is part of `core:window:default`, so this needs no extra
 *  capability. Defaults to focused: an unlit caption strip on a focused window
 *  is a worse lie than a missed dim. */
export function useWindowFocused(): boolean {
  const [focused, setFocused] = useState(true);

  useEffect(() => {
    let disposed = false;
    let unlisten: (() => void) | null = null;

    const setup = async () => {
      try {
        const win = getCurrentWindow();
        const initial = await win.isFocused();
        if (disposed) return;
        setFocused(initial);
        const stop = await win.onFocusChanged(({ payload }) => {
          // A late event after unmount must be a no-op; the handler outlives
          // this effect's scope, so it re-checks `disposed`.
          if (!disposed) setFocused(payload);
        });
        // Subscribing is awaited too, so unmount can land between the call and
        // its resolution — by which point the cleanup has already run with
        // nothing to tear down. Without this the listener would outlive the
        // component; React 19 StrictMode's mount → cleanup → mount makes that
        // interleaving routine rather than exotic.
        if (disposed) {
          stop();
          return;
        }
        unlisten = stop;
      } catch (e) {
        console.warn('Failed to track window focus:', e);
      }
    };

    void setup();

    return () => {
      disposed = true;
      if (unlisten) unlisten();
    };
  }, []);

  return focused;
}
