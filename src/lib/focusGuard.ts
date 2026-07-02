/**
 * Whether `document.activeElement` is a real text-input field that the user
 * is currently typing into — vs. a keyboard-capture surface (xterm.js) or
 * the page body. Used by global-shortcut handlers in `src/App.tsx` to
 * avoid swallowing keystrokes the user meant for an input.
 *
 * Two escape hatches the spec doesn't spell out but the deployment surface
 * needs:
 *
 * 1. xterm.js attaches a hidden `<textarea class="xterm-helper-textarea"
 *    aria-hidden="true">` to capture keystrokes when the terminal canvas
 *    is focused. Without the carve-out, Alt+G would silently fail in the
 *    one state users actually want it (typing a prompt in an agent
 *    terminal). The user's typing into the terminal canvas, not into the
 *    textarea. We discriminate via `aria-hidden="true"` (the spec signal)
 *    and the className (belt-and-braces for libraries that drop the aria
 *    attr).
 *
 * 2. xterm-helper-textarea also flips to `tabindex=0` on focus, so it
 *    really is `document.activeElement` while the terminal has focus —
 *    matching `tagName === 'TEXTAREA'` on its own would block the
 *    shortcut.
 */
export function isTextInputFocused(): boolean {
  const el = document.activeElement as HTMLElement | null;
  if (!el) return false;
  if (el.getAttribute('aria-hidden') === 'true') return false;
  if (el.classList.contains('xterm-helper-textarea')) return false;
  const tag = el.tagName;
  if (tag === 'INPUT' || tag === 'TEXTAREA' || tag === 'SELECT') return true;
  return el.isContentEditable === true;
}

/**
 * Inverse-direction companion to `isTextInputFocused`: returns true when
 * the focused element is an xterm.js helper textarea, meaning the user is
 * actively typing in an agent terminal.
 *
 * Used by shortcut handlers whose binding the user is more likely to want
 * to send *to the agent* than to the app — e.g. the `?`-key cheatsheet
 * toggle (issue #731). The Alt+G grid-toggle uses the opposite signal
 * (the `isTextInputFocused` xterm carve-out): grid-toggle should fire
 * from a terminal prompt, cheatsheet-toggle should NOT fire from a
 * terminal prompt (the user's `?` is a question for the agent, not a
 * help request). Same xterm-helper-textarea detection; opposite policy.
 */
export function isTerminalFocused(): boolean {
  const el = document.activeElement as HTMLElement | null;
  if (!el) return false;
  return el.classList.contains('xterm-helper-textarea');
}