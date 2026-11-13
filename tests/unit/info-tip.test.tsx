/**
 * InfoTip — the shared contextual-help popover (issue #2773).
 *
 * The tip used to be an absolutely positioned child of the trigger, so
 * inside the settings scroller it was clipped by that ancestor's
 * `overflow` and most of the explanation was unreadable. It also toggled
 * on click, which meant hovering the icon and then clicking it closed the
 * tooltip the pointer had just opened.
 *
 * These tests pin the behaviour a user notices:
 *
 *   1. Hover reveals the tip; the pointer path from the trigger into the
 *      panel keeps it open.
 *   2. A click pins the tip open — it never closes the tip the pointer
 *      just hovered open — and a second click closes it again.
 *   3. A click with no hover opens the tip (the only way to open it on a
 *      touch device).
 *   4. Escape closes the tip and returns focus to the trigger; an outside
 *      click closes it.
 *   5. The panel is rendered outside the clipping ancestor.
 *   6. The panel is placed inside the viewport: a trigger near the
 *      bottom flips the panel above it and bounds its height.
 */
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, screen, fireEvent, cleanup, waitFor, act } from '@testing-library/react';
import { InfoTip } from '../../src/components/shared/InfoTip';

const HOVER_CLOSE_DELAY_MS = 200;

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  vi.restoreAllMocks();
});

function renderTip() {
  return render(
    <InfoTip label="Window zoom" triggerTestId="zoom-trigger" testId="zoom-panel">
      Zoom scales the interface text.
    </InfoTip>,
  );
}

function trigger(): HTMLElement {
  return screen.getByTestId('zoom-trigger');
}

/** The trigger's wrapper — the element that owns the hover handlers. */
function triggerWrapper(): HTMLElement {
  return trigger().parentElement as HTMLElement;
}

function panel(): HTMLElement | null {
  return screen.queryByTestId('zoom-panel');
}

describe('InfoTip', () => {
  it('reveals the tip on hover', () => {
    renderTip();
    expect(panel()).toBeNull();
    fireEvent.mouseEnter(triggerWrapper());
    expect(panel()).not.toBeNull();
  });

  it('keeps the tip open when the pointer hovers it and then clicks it', () => {
    // The regression: hover opened the tip and the click toggled it shut.
    renderTip();
    fireEvent.mouseEnter(triggerWrapper());
    expect(panel()).not.toBeNull();
    fireEvent.click(trigger());
    expect(panel()).not.toBeNull();
  });

  it('keeps a pinned tip open after the pointer leaves the trigger', () => {
    vi.useFakeTimers();
    renderTip();
    fireEvent.mouseEnter(triggerWrapper());
    fireEvent.click(trigger());
    fireEvent.mouseLeave(triggerWrapper());
    act(() => { vi.advanceTimersByTime(HOVER_CLOSE_DELAY_MS * 4); });
    expect(panel()).not.toBeNull();
  });

  it('closes an unpinned tip after the pointer leaves it', () => {
    vi.useFakeTimers();
    renderTip();
    fireEvent.mouseEnter(triggerWrapper());
    fireEvent.mouseLeave(triggerWrapper());
    act(() => { vi.advanceTimersByTime(HOVER_CLOSE_DELAY_MS * 2); });
    expect(panel()).toBeNull();
  });

  it('opens on a click with no hover, and closes on a second click', () => {
    // The click-only path a touch device has.
    renderTip();
    fireEvent.click(trigger());
    expect(panel()).not.toBeNull();
    fireEvent.click(trigger());
    expect(panel()).toBeNull();
  });

  it('keeps the tip open while the pointer travels from the trigger into the panel', () => {
    vi.useFakeTimers();
    renderTip();
    fireEvent.mouseEnter(triggerWrapper());
    // The pointer leaves the trigger and crosses the gap between the two.
    fireEvent.mouseLeave(triggerWrapper());
    act(() => { vi.advanceTimersByTime(HOVER_CLOSE_DELAY_MS / 2); });
    expect(panel()).not.toBeNull();
    // It arrives in the panel, which cancels the pending close.
    fireEvent.mouseEnter(panel() as HTMLElement);
    act(() => { vi.advanceTimersByTime(HOVER_CLOSE_DELAY_MS * 4); });
    expect(panel()).not.toBeNull();
  });

  it('closes on Escape and returns focus to the trigger', async () => {
    renderTip();
    fireEvent.click(trigger());
    expect(panel()).not.toBeNull();
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(panel()).toBeNull());
    await waitFor(() => expect(document.activeElement).toBe(trigger()));
  });

  it('closes on an outside click', () => {
    renderTip();
    fireEvent.click(trigger());
    expect(panel()).not.toBeNull();
    fireEvent.mouseDown(document.body);
    expect(panel()).toBeNull();
  });

  it('does not close when the click lands inside the panel', () => {
    renderTip();
    fireEvent.click(trigger());
    fireEvent.mouseDown(panel() as HTMLElement);
    expect(panel()).not.toBeNull();
  });

  it('renders the panel outside the scrolling ancestor that used to clip it', () => {
    render(
      <div data-testid="scroller" style={{ overflowY: 'auto', height: 100 }}>
        <InfoTip label="Theme" triggerTestId="theme-trigger" testId="theme-panel">
          Theme explanation.
        </InfoTip>
      </div>,
    );
    fireEvent.mouseEnter(screen.getByTestId('theme-trigger').parentElement as HTMLElement);
    const panel = screen.getByTestId('theme-panel');
    // The panel is portalled to the body, so the settings scroller cannot
    // clip it — a higher z-index alone could not have fixed that.
    expect(panel.parentElement).toBe(document.body);
    expect(screen.getByTestId('scroller').contains(panel)).toBe(false);
  });

  it('places the panel inside the viewport when the trigger is near the bottom', () => {
    const originalWidth = Object.getOwnPropertyDescriptor(window, 'innerWidth');
    const originalHeight = Object.getOwnPropertyDescriptor(window, 'innerHeight');
    const originalScrollHeight = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'scrollHeight');
    Object.defineProperty(window, 'innerWidth', { configurable: true, value: 900 });
    Object.defineProperty(window, 'innerHeight', { configurable: true, value: 600 });
    // The panel's content is taller than the room below the trigger.
    Object.defineProperty(HTMLElement.prototype, 'scrollHeight', { configurable: true, get: () => 200 });

    vi.spyOn(HTMLElement.prototype, 'getBoundingClientRect').mockImplementation(function (this: HTMLElement) {
      if (this.dataset.testid === 'zoom-trigger') {
        return { top: 500, bottom: 524, left: 700, right: 724, width: 24, height: 24, x: 700, y: 500, toJSON: () => ({}) } as DOMRect;
      }
      return { top: 0, bottom: 0, left: 0, right: 288, width: 288, height: 200, x: 0, y: 0, toJSON: () => ({}) } as DOMRect;
    });

    try {
      renderTip();
      fireEvent.mouseEnter(triggerWrapper());
      const panel = screen.getByTestId('zoom-panel');
      const top = Number.parseFloat(panel.style.top);
      const left = Number.parseFloat(panel.style.left);
      const maxHeight = Number.parseFloat(panel.style.maxHeight);
      // Opened above the trigger rather than running off the bottom.
      expect(top).toBeLessThan(500);
      expect(top + 200).toBeLessThanOrEqual(600 - 8);
      // Flipped to the trigger's other edge rather than running off the
      // right-hand edge.
      expect(left + 288).toBeLessThanOrEqual(900 - 8);
      expect(maxHeight).toBeGreaterThan(0);
    } finally {
      if (originalWidth) Object.defineProperty(window, 'innerWidth', originalWidth);
      if (originalHeight) Object.defineProperty(window, 'innerHeight', originalHeight);
      if (originalScrollHeight) Object.defineProperty(HTMLElement.prototype, 'scrollHeight', originalScrollHeight);
    }
  });
});
