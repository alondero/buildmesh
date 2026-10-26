import { WebLinksAddon } from '@xterm/addon-web-links';
import type { ILinkHandler, Terminal } from '@xterm/xterm';

type Link = { uri: string; range: string };
const TAP_SLOP_PX = 8;
const TAP_TIMEOUT_MS = 500;

/** Plain URLs and OSC 8 links share one touch gesture and a platform-owned opener. */
export function attachMobileTerminalLinks(term: Terminal, openUrl: (uri: string) => void): () => void {
  const element = term.element;
  if (!element) return () => {};

  let hovered: Link | null = null;
  let pressed: { id: number; x: number; y: number; time: number; link: Link } | null = null;
  const activate = (_event: MouseEvent, uri: string) => {
    // Terminal output is untrusted. Keep both platform openers HTTP(S)-only.
    let url: URL;
    try {
      url = new URL(uri);
    } catch {
      return;
    }
    if (url.protocol === 'http:' || url.protocol === 'https:') openUrl(uri);
  };
  const hover: NonNullable<ILinkHandler['hover']> = (_event, uri, range) => {
    hovered = { uri, range: JSON.stringify(range) };
  };
  const leave = () => { hovered = null; };
  const previousHandler = term.options.linkHandler;
  term.options.linkHandler = { activate, hover, leave };
  const webLinks = new WebLinksAddon(activate, { hover, leave });
  term.loadAddon(webLinks);

  const linkAt = (touch: Touch): Link | null => {
    const screenElement = element.querySelector('.xterm-screen');
    const screen = screenElement?.getBoundingClientRect();
    if (!screen || touch.clientX < screen.left || touch.clientX >= screen.right ||
      touch.clientY < screen.top || touch.clientY >= screen.bottom) return null;

    // xterm's public link providers resolve on mousemove, not touch. Move to
    // another cell first so repeated taps also re-resolve after output/reflow.
    // Keep probes on the screen: bubbling would reach xterm's PTY mouse
    // reporting on the parent element and send input to the host computer.
    const move = (clientX: number) => screenElement!.dispatchEvent(new MouseEvent('mousemove', {
      clientX, clientY: touch.clientY,
    }));
    move(touch.clientX < screen.left + screen.width / 2 ? screen.right - 1 : screen.left + 1);
    move(touch.clientX);
    return hovered;
  };
  const onStart = (event: TouchEvent) => {
    pressed = null;
    if (event.touches.length !== 1) return;
    const touch = event.touches[0];
    const link = linkAt(touch);
    if (link) pressed = { id: touch.identifier, x: touch.clientX, y: touch.clientY, time: event.timeStamp, link };
  };
  const onMove = (event: TouchEvent) => {
    if (!pressed) return;
    const touch = event.touches[0];
    if (event.touches.length !== 1 || touch.identifier !== pressed.id ||
      Math.hypot(touch.clientX - pressed.x, touch.clientY - pressed.y) > TAP_SLOP_PX) pressed = null;
  };
  const onEnd = (event: TouchEvent) => {
    const start = pressed;
    pressed = null;
    if (!start || event.touches.length !== 0) return;
    // Suppress compatibility clicks even for an expired long press; otherwise
    // xterm would activate it through its ordinary mouse handler afterwards.
    event.preventDefault();
    const touch = event.changedTouches[0];
    if (!touch || touch.identifier !== start.id || event.timeStamp - start.time > TAP_TIMEOUT_MS ||
      Math.hypot(touch.clientX - start.x, touch.clientY - start.y) > TAP_SLOP_PX) return;
    const link = linkAt(touch);
    if (link?.uri !== start.link.uri || link.range !== start.link.range) return;
    // Open synchronously while touchend still permits a browser tab.
    activate(new MouseEvent('click'), link.uri);
  };
  const onCancel = () => { pressed = null; };

  element.addEventListener('touchstart', onStart, { passive: true });
  element.addEventListener('touchmove', onMove, { passive: true });
  element.addEventListener('touchend', onEnd, { passive: false });
  element.addEventListener('touchcancel', onCancel, { passive: true });
  return () => {
    element.removeEventListener('touchstart', onStart);
    element.removeEventListener('touchmove', onMove);
    element.removeEventListener('touchend', onEnd);
    element.removeEventListener('touchcancel', onCancel);
    webLinks.dispose(); // allow-dispose — addon registration, not the terminal
    term.options.linkHandler = previousHandler;
  };
}
