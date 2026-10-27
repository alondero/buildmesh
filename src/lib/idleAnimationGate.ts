/**
 * The idle animation gate (issue #2599).
 *
 * ## Why it exists
 *
 * The WebView renderer's **Compositor** thread produces a frame every vsync
 * while ANY infinite CSS animation runs — even when the window is visible but
 * not focused and nothing on the page has changed. On a long-running hub that
 * measured ~16 ms per frame at 4K: the compositor thread stayed pinned near
 * one core for hours while the JavaScript main thread sat at 3%. The page
 * never went idle, because an infinite animation never ends.
 *
 * `scripts/idle-animation-probe.mjs` measures the frame half of that claim on
 * the real frontend: with the animations running the trace records ~60
 * compositor frames per second; with the gate closed it records none. The
 * CPU half depends on the machine — a shared, busy host attributes time to
 * the renderer that has nothing to do with it — so the frames are the
 * measurement, and the CPU is the corroboration.
 *
 * The fix is not to remove the animations — they carry the attention and
 * liveness vocabulary — but to stop *running* them while nobody is looking.
 * A paused animation keeps its current phase, so the glyph the user comes
 * back to is the glyph they left, and the state cue (shape, colour, label)
 * is unchanged.
 *
 * ## The contract
 *
 * `data-window-idle="true"` on the root element means "the app is not the
 * foreground surface". The attribute is ABSENT otherwise, so the default — a
 * page that never wires the gate up — keeps every animation running. Absent
 * is the safe default: an app that fails to notice it lost focus looks
 * exactly like it does today, while an app that wrongly believes it lost
 * focus would silently stop showing live state.
 *
 * One rule in `App.css` reads this attribute and pauses the decorative
 * infinite animations. The one-shot entrance animations are deliberately not
 * in that rule: a toast that fades in while the window is in the background
 * would freeze at zero opacity and never be seen.
 */

/** Root attribute that marks the app as not being the foreground surface. */
export const IDLE_ROOT_ATTRIBUTE = 'data-window-idle';

/**
 * Whether the app counts as idle.
 *
 * Two independent facts make the app idle: the OS window lost focus (the user
 * is in another application), or the document is hidden (the window was
 * minimised). Either one means nobody is looking at the page, so the
 * compositor has no reason to keep producing frames for it.
 */
export function isWindowIdle(focused: boolean, visibilityState: string): boolean {
  return !focused || visibilityState === 'hidden';
}

/**
 * Write the idle state onto the root element.
 *
 * The attribute is set to `'true'` while idle and removed otherwise, so the
 * stylesheet's `[data-window-idle='true']` selector matches only in the
 * idle case and the default page needs no attribute at all.
 */
export function applyWindowIdle(idle: boolean): void {
  const root = document.documentElement;
  if (!root) return;
  if (idle) {
    root.setAttribute(IDLE_ROOT_ATTRIBUTE, 'true');
  } else {
    root.removeAttribute(IDLE_ROOT_ATTRIBUTE);
  }
}
