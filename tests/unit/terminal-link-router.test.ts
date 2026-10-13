import { describe, it, expect, vi, beforeEach } from 'vitest';
import { openUrl } from '@tauri-apps/plugin-opener';
import type { Terminal } from '@xterm/xterm';
import { TerminalLinkRouter } from '../../src/components/Terminal/TerminalLinkRouter';

vi.mock('@tauri-apps/plugin-opener', () => ({
  openUrl: vi.fn().mockResolvedValue(undefined),
}));

const URI = 'https://example.com/codex-link';
const OTHER_URI = 'https://example.com/other-link';
const RANGE = { start: { x: 1, y: 1 }, end: { x: 26, y: 1 } };

/**
 * Stands in for the xterm surface the router touches. `screenPresses` and
 * `screenReleases` model what xterm can see: its own listeners live on the
 * screen element, so a press or release the router withholds never reaches the
 * linkifier (and so can never activate) and never reaches the PTY.
 */
function mount(initialTracking: 'none' | 'x10' | 'vt200' | 'drag' | 'any') {
  const element = document.createElement('div');
  const screen = document.createElement('div');
  element.appendChild(screen);
  document.body.appendChild(element);

  const screenPresses: MouseEvent[] = [];
  const screenReleases: MouseEvent[] = [];
  screen.addEventListener('mousedown', (e) => screenPresses.push(e));
  screen.addEventListener('mouseup', (e) => screenReleases.push(e));

  const focus = vi.fn();
  const term = {
    element,
    modes: { mouseTrackingMode: initialTracking },
    focus,
  } as unknown as Terminal;

  const router = new TerminalLinkRouter();
  router.attach(term);

  const press = (button = 0) =>
    screen.dispatchEvent(new MouseEvent('mousedown', { bubbles: true, button }));
  const release = (button = 0, target: HTMLElement = screen) =>
    target.dispatchEvent(new MouseEvent('mouseup', { bubbles: true, button }));
  const hoverLink = (uri = URI) => router.linkHandler.hover?.(new MouseEvent('mousemove'), uri, RANGE);
  const leaveLink = () => router.linkHandler.leave?.(new MouseEvent('mousemove'), URI, RANGE);
  const setTracking = (mode: 'none' | 'any') => {
    (term.modes as { mouseTrackingMode: string }).mouseTrackingMode = mode;
  };
  const click = (button = 0) => {
    press(button);
    release(button);
  };

  return {
    router, term, focus, press, release, click, hoverLink, leaveLink, setTracking,
    screenPresses, screenReleases, element, screen,
  };
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
    const { click, hoverLink, focus, screenPresses, screenReleases } = mount('any');
    hoverLink();

    click();

    expect(screenPresses).toHaveLength(0);
    expect(screenReleases).toHaveLength(0);
    expect(openUrl).toHaveBeenCalledExactlyOnceWith(URI);
    // xterm's own mousedown handler focuses the terminal before reporting the
    // press; withholding the press must not cost the focus.
    expect(focus).toHaveBeenCalledOnce();
  });

  // xterm re-resolves the hovered link only when the pointer moves to another
  // cell, so a second click on the same link produces no further hover event.
  // Clearing the tracked URI on the first click let that press through to the
  // agent, which opened the link itself while xterm activated it on release.
  it('keeps arbitrating repeated clicks on one link with no pointer movement', () => {
    const { click, hoverLink, screenPresses, screenReleases } = mount('any');
    hoverLink();

    click();
    click();
    click();

    expect(screenPresses).toHaveLength(0);
    expect(screenReleases).toHaveLength(0);
    expect(openUrl).toHaveBeenCalledTimes(3);
  });

  // xterm records the link on mousedown and never clears that record after
  // activating, so a link clicked while the agent was not tracking the mouse
  // leaves a release able to activate. A later click on that same link must
  // not reach xterm's release.
  it('withholds the release after a link click from a non-tracking agent', () => {
    const { click, hoverLink, screenPresses, screenReleases, setTracking } = mount('none');
    hoverLink();

    click();
    expect(screenPresses).toHaveLength(1);
    expect(screenReleases).toHaveLength(1);

    // The agent turns mouse tracking on (Codex does this at startup), and the
    // user clicks the same link again.
    setTracking('any');
    screenPresses.length = 0;
    screenReleases.length = 0;
    vi.mocked(openUrl).mockClear();

    click();

    expect(screenPresses).toHaveLength(0);
    expect(screenReleases).toHaveLength(0);
    expect(openUrl).toHaveBeenCalledExactlyOnceWith(URI);
  });

  // xterm activates only when the press and the release are on the same link.
  it('does not open when the gesture is released off the link', () => {
    const { press, release, hoverLink, leaveLink, screenReleases } = mount('any');
    hoverLink();

    press();
    // Drag onto different text: the linkifier resolves a leave as the pointer
    // crosses into another cell.
    leaveLink();
    release();

    expect(openUrl).not.toHaveBeenCalled();
    expect(screenReleases).toHaveLength(0);
  });

  it('does not open when the gesture is released on a different link', () => {
    const { press, release, hoverLink } = mount('any');
    hoverLink();

    press();
    hoverLink(OTHER_URI);
    release();

    expect(openUrl).not.toHaveBeenCalled();
  });

  it('opens on the release rather than the press', () => {
    const { press, release, hoverLink } = mount('any');
    hoverLink();

    press();
    expect(openUrl).not.toHaveBeenCalled();

    release();
    expect(openUrl).toHaveBeenCalledExactlyOnceWith(URI);
  });

  // A release outside this terminal ends the gesture without an open, and the
  // event is left for whatever the release landed on.
  it('does not interfere with a release outside the terminal', () => {
    const outside = document.createElement('button');
    document.body.appendChild(outside);
    const outsideReleases: MouseEvent[] = [];
    outside.addEventListener('mouseup', (e) => outsideReleases.push(e));
    const { press, release, hoverLink } = mount('any');
    hoverLink();

    press();
    release(0, outside);

    expect(openUrl).not.toHaveBeenCalled();
    expect(outsideReleases).toHaveLength(1);
  });

  it('leaves a press off the link alone so the agent still gets its click', () => {
    const { click, focus, screenPresses, screenReleases } = mount('any');

    click();

    expect(screenPresses).toHaveLength(1);
    expect(screenReleases).toHaveLength(1);
    expect(openUrl).not.toHaveBeenCalled();
    expect(focus).not.toHaveBeenCalled();
  });

  // Codex binds right-click to paste, so a secondary press over a link is the
  // agent's business even though a left press there would be ours.
  it('leaves a non-primary press on the link to the agent', () => {
    const { press, hoverLink, screenPresses } = mount('any');
    hoverLink();

    press(2);

    expect(screenPresses).toHaveLength(1);
    expect(openUrl).not.toHaveBeenCalled();
  });

  // Harnesses that never enable mouse tracking never forwarded the press, so
  // their links must keep xterm's activate-on-release path untouched.
  it('does not intercept the gesture when the agent does not track the mouse', () => {
    const { click, hoverLink, screenPresses, screenReleases } = mount('none');
    hoverLink();

    click();

    expect(screenPresses).toHaveLength(1);
    expect(screenReleases).toHaveLength(1);
    expect(openUrl).not.toHaveBeenCalled();
  });

  it('opens the OSC 8 activation itself when the gesture was not intercepted', () => {
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
    const { click, router, screenPresses, screenReleases } = mount('vt200');

    router.webLinksOptions.hover?.(new MouseEvent('mousemove'), URI, RANGE);
    click();
    expect(openUrl).toHaveBeenCalledExactlyOnceWith(URI);

    vi.mocked(openUrl).mockClear();
    router.webLinksOptions.leave?.(new MouseEvent('mousemove'), URI);
    click();
    expect(openUrl).not.toHaveBeenCalled();
    expect(screenPresses).toHaveLength(1);
    expect(screenReleases).toHaveLength(1);
  });

  it('stops arbitrating after dispose', () => {
    const { router, click, hoverLink, screenPresses, screenReleases } = mount('any');
    hoverLink();

    router.dispose();
    click();

    expect(screenPresses).toHaveLength(1);
    expect(screenReleases).toHaveLength(1);
    expect(openUrl).not.toHaveBeenCalled();
  });

  it('does not stack a second listener when a re-attach rebinds the same terminal', () => {
    const { router, term, click, hoverLink, screenPresses } = mount('any');

    router.attach(term); // re-attach re-parents; the element is the same one
    hoverLink();
    click();

    expect(openUrl).toHaveBeenCalledExactlyOnceWith(URI);
    expect(screenPresses).toHaveLength(0);
  });

  // The hovered URI is the router's only view of "the press is on a link", and
  // xterm re-resolves it on mousemove only. A stale hover must not outlive the
  // link it named.
  it('stops arbitrating once the pointer leaves the link', () => {
    const { click, hoverLink, leaveLink, screenPresses } = mount('any');
    hoverLink();
    leaveLink();

    click();

    expect(screenPresses).toHaveLength(1);
    expect(openUrl).not.toHaveBeenCalled();
  });
});