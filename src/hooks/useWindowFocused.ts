import { useEffect, useState } from 'react';
import { watchWindowFocus } from '../lib/windowFocus';

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
 *  is a worse lie than a missed dim.
 *
 *  The OS focus fact is read through one implementation, in
 *  `lib/windowFocus`, so the consumers of that fact answer "does the window
 *  have focus" the same way: this hook feeds the caption buttons' dimming
 *  (ADR-0035) and the macOS traffic lights (ADR-0036), and the idle animation
 *  gate reads the same module (issue #2599). */
export function useWindowFocused(): boolean {
  const [focused, setFocused] = useState(true);

  useEffect(() => watchWindowFocus(setFocused), []);

  return focused;
}
