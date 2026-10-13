import { describe, it, expect, vi, beforeEach } from 'vitest';
import { openUrl } from '@tauri-apps/plugin-opener';
import type { Terminal } from '@xterm/xterm';
import { TerminalLinkRouter } from '../../src/components/Terminal/TerminalLinkRouter';

vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: vi.fn().mockResolvedValue(undefined),
}));

const URI = 'https://example.com/codex-link';
const RANGE = { start: { x: 1, y: 1 }, end: { x: 26, y: 1 } };

/**
 * Stands in for the xterm surface the router touches. The PTY side is
 * modelled by a `mousedown` listener on the screen element — the same place
 * xterm hangs the listener that reports the press to the agent's TUI — so a
 * swallowed press is observable as "the agent never saw it".
 */
function mount(mouseTrackingMode: 'none' | 'x10' | 'vt200' | 'drag' | 'any') {
  const element = document.createElement('div');
  const screen = document.createElement('div');
  element.appendChild(screen);
  document.body.appendChild(element);

  const ptyPresses: MouseEvent[] = [];
  screen.addEventListener('mousedown', (e) => ptyPresses.push(e));

  const focus = vi.fn();
  const term = {
    element,
    modes: { mouseTrackingMode },
    focus,
  } as unknown as Terminal;

  const router = new TerminalLinkRouter();
  router.attach(term);

  const press = (button = 0) =>
    screen.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, button }));
  const hoverLink = () => router.linkHandler.hover?.(new MouseEvent('mousemove'), URI, RANGE);
  const leaveLink = () => router.linkHandler.leave?.(new MouseEvent('mousemove'), URI, RANGE);

  return { router, term, focus, press, hoverLink, leaveLink, ptyPresses, element, screen };
}

describe('TerminalLinkRouter', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    document.body.innerHTML = '';
  });

  // Regression: a click on a Codex OSC 8 link used to open the URL twice —
  // once here and once by the agent's own TUI, which received the same press
  // as an SGR mouse report and opened the hyperlink itself.
  it('keeps a press on a link from reaching a mouse-tracking agent, and opens once', () => {
    const { press, hoverLink, focus, ptyPresses } = mount('any');
    hoverLink();

    press();

    expect(ptyPresses).toHaveLength(0);
    expect(openUrl).toHaveBeenCalledExactlyOnceWith(URI);
    // xterm's own mousedown handler focuses the terminal before reporting the
    // press; swallowing the press must not cost the focus.
    expect(focus).toHaveBeenCalledOnce();
  });

  it('leaves a press off the link alone so the agent still gets its click', () => {
    const { press, focus, ptyPresses } = mount('any');

    press();

    expect(ptyPresses).toHaveLength(1);
    expect(openUrl).not.toHaveBeenCalled();
    expect(focus).not.toHaveBeenCalled();
  });

  // Harnesses that never enable mouse tracking never forwarded the press, so
  // their links must keep xterm's activate-on-release path untouched.
  it('does not intercept the press when the agent does not track the mouse', () => {
    const { press, hoverLink, ptyPresses } = mount('none');
    hoverLink();

    press();

    expect(ptyPresses).toHaveLength(1);
    expect(openUrl).not.toHaveBeenCalled();
  });

  // Codex binds right-click to paste, so a secondary press over a link is the
  // agent's business even though a left press there would be ours.
  it('leaves a non-primary press on the link to the agent', () => {
    const { press, hoverLink, ptyPresses } = mount('any');
    hoverLink();

    press(2);

    expect(ptyPresses).toHaveLength(1);
    expect(openUrl).not.toHaveBeenCalled();
  });

  it('opens the OSC 8 activation itself when the press was not intercepted', () => {
    const { router } = mount('none');

    router.linkHandler.activate(new MouseEvent('mouseup'), URI, RANGE);

    expect(openUrl).toHaveBeenCalledExactlyOnceWith(URI);
  });

  it('opens a plain printed URL through the WebLinks handler', () => {
    const { router } = mount('none');

    router.openWebLink(new MouseEvent('mouseup'), URI);

    expect(openUrl).toHaveBeenCalledExactlyOnceWith(URI);
  });

  it('routes the WebLinks hover/leave pair through the same tracking', () => {
    const { router, press, ptyPresses } = mount('vt200');

    router.webLinksOptions.hover?.(new MouseEvent('mousemove'), URI, RANGE);
    press();
    expect(ptyPresses).toHaveLength(0);

    router.webLinksOptions.leave?.(new MouseEvent('mousemove'), URI);
    press();
    expect(ptyPresses).toHaveLength(1);
  });

  it('stops intercepting after dispose', () => {
    const { router, press, hoverLink, ptyPresses } = mount('any');
    hoverLink();

    router.dispose();
    press();

    expect(ptyPresses).toHaveLength(1);
    expect(openUrl).not.toHaveBeenCalled();
  });

  it('does not stack a second listener when a re-attach rebinds the same terminal', () => {
    const { router, term, press, hoverLink, ptyPresses } = mount('any');

    router.attach(term); // re-attach re-parents; the element is the same one
    hoverLink();
    press();

    expect(ptyPresses).toHaveLength(0);
    expect(openUrl).toHaveBeenCalledExactlyOnceWith(URI);
  });

  // The hovered URI is the router's only view of "the press is on a link", and
  // xterm re-resolves it on mousemove only. A stale hover must not outlive the
  // link it named.
  it('stops intercepting once the pointer leaves the link', () => {
    const { press, hoverLink, leaveLink, ptyPresses } = mount('any');
    hoverLink();
    leaveLink();

    press();

    expect(ptyPresses).toHaveLength(1);
    expect(openUrl).not.toHaveBeenCalled();
  });
});