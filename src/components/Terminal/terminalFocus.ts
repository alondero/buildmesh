/**
 * Whether a terminal that has just finished attaching may take keyboard focus.
 *
 * Activating a node asks for focus immediately. The xterm chunk can still be
 * loading, so the request is remembered as the element that held focus and
 * applied when `attach` resolves. It is applied when that element is still
 * focused, or when focus is on the document body or already inside the
 * terminal. A cold mount has no remembered element: it takes focus only when
 * the body or the terminal itself holds it. A control the user reached in
 * the meantime, such as the Probe scope disclosure, keeps focus.
 */
export interface DeferredTerminalFocus {
  target: Element | null;
}

export function rememberTerminalFocusRequest(): DeferredTerminalFocus {
  return { target: document.activeElement };
}

export function shouldAdoptTerminalFocus(
  container: HTMLElement,
  deferred: DeferredTerminalFocus | null,
): boolean {
  const active = document.activeElement;
  const outside = active instanceof Element
    && active !== document.body
    && !container.contains(active);
  if (deferred) return active === deferred.target || !outside;
  return !outside;
}
