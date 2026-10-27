import { useEffect } from 'react';
import { watchWindowFocus } from '../lib/windowFocus';
import { applyWindowIdle, isWindowIdle } from '../lib/idleAnimationGate';

/**
 * Pause the decorative infinite CSS animations while the app is not the
 * foreground surface (issue #2599).
 *
 * The renderer Compositor thread produces a frame every vsync while an
 * infinite CSS animation runs, so a window that is visible but unfocused
 * keeps burning CPU for hours even when nothing on the page changed — the
 * hub case in issue #2599 measured the compositor thread pinned near one
 * core. This hook marks the root element as idle when the OS window loses
 * focus or the document is hidden; `App.css` reads that marker and pauses the
 * animations. `scripts/idle-animation-probe.mjs` measures the effect: the
 * renderer's compositor frames go from ~60 per second to none.
 *
 * It writes the marker as a DOM side effect and holds NO React state, on
 * purpose: a focus change must not re-render the workspace. The grid holds
 * live terminals, and re-rendering it on every alt-tab would trade a
 * compositor problem for a React problem.
 */
export function useIdleAnimationGate(): void {
  useEffect(() => {
    // Optimistic until the OS answers: focused and visible means "run the
    // animations", which is how the app behaves today. A failed focus query
    // leaves the page exactly as it was.
    let focused = true;

    const apply = () => {
      applyWindowIdle(isWindowIdle(focused, document.visibilityState));
    };

    const stopFocus = watchWindowFocus((next) => {
      focused = next;
      apply();
    });

    const onVisibilityChange = () => {
      apply();
    };
    document.addEventListener('visibilitychange', onVisibilityChange);

    // The document may already be hidden when the app mounts (a window
    // restored from the taskbar minimised, or a window opened behind another
    // one), and no visibilitychange event is delivered for a state the page
    // was already in when it started.
    apply();

    return () => {
      stopFocus();
      document.removeEventListener('visibilitychange', onVisibilityChange);
      // The marker belongs to this effect. Leaving it behind would pause the
      // animations of whatever mounts next.
      applyWindowIdle(false);
    };
  }, []);
}
