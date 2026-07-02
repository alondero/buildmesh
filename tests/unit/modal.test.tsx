/**
 * Tests for the shared <Modal> primitive (design-system pass).
 *
 * Every dialog in the app previously hand-rolled its own shell and they had
 * drifted: only one of four handled Escape, none set dialog ARIA, none moved
 * focus. These tests pin the contract the four retrofitted modals now rely on.
 *
 * The dirty-check tests at the bottom pin the issue #730 contract: a half-typed
 * form is not silently destroyed by a stray backdrop click or Escape.
 */
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, fireEvent } from '@testing-library/react';
import { Modal } from '../../src/components/shared/Modal';

afterEach(cleanup);

describe('Modal', () => {
  it('renders dialog semantics (role, aria-modal, label wiring)', () => {
    const { getByRole } = render(
      <Modal onClose={() => {}} labelledBy="t">
        <h2 id="t">Title</h2>
      </Modal>,
    );
    const dialog = getByRole('dialog');
    expect(dialog.getAttribute('aria-modal')).toBe('true');
    expect(dialog.getAttribute('aria-labelledby')).toBe('t');
  });

  it('closes on Escape — the dismiss path that works even when the WebView is occluded (issue #643)', () => {
    const onClose = vi.fn();
    render(
      <Modal onClose={onClose} ariaLabel="test">
        <p>body</p>
      </Modal>,
    );
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('detaches the Escape listener on unmount so it cannot steal Escape from agent terminals', () => {
    const onClose = vi.fn();
    const { unmount } = render(
      <Modal onClose={onClose} ariaLabel="test">
        <p>body</p>
      </Modal>,
    );
    unmount();
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onClose).not.toHaveBeenCalled();
  });

  it('closes on backdrop click by default but not when closeOnBackdrop is false', () => {
    const onClose = vi.fn();
    const { container, rerender } = render(
      <Modal onClose={onClose} ariaLabel="test">
        <p>body</p>
      </Modal>,
    );
    // The visible dimmer is the wrapper's first child (an absolute-positioned
    // <div className="absolute inset-0 bg-bg-base/70 backdrop-blur-sm" />).
    // A real user clicks the dimmer, not the wrapper. Earlier versions of
    // this test clicked the wrapper and gave false green coverage when the
    // handler had a `e.target !== e.currentTarget` guard (code-review catch).
    const dimmer = (container.firstElementChild as HTMLElement).firstElementChild as HTMLElement;
    fireEvent.click(dimmer);
    expect(onClose).toHaveBeenCalledTimes(1);

    onClose.mockClear();
    rerender(
      <Modal onClose={onClose} ariaLabel="test" closeOnBackdrop={false}>
        <p>body</p>
      </Modal>,
    );
    fireEvent.click((container.firstElementChild as HTMLElement).firstElementChild as HTMLElement);
    expect(onClose).not.toHaveBeenCalled();
  });

  it('does not close when clicking inside the panel', () => {
    const onClose = vi.fn();
    const { getByText } = render(
      <Modal onClose={onClose} ariaLabel="test">
        <p>body</p>
      </Modal>,
    );
    fireEvent.click(getByText('body'));
    expect(onClose).not.toHaveBeenCalled();
  });

  it('moves focus into the dialog on mount and restores it on unmount', () => {
    const outside = document.createElement('button');
    document.body.appendChild(outside);
    outside.focus();
    expect(document.activeElement).toBe(outside);

    const { getByRole, unmount } = render(
      <Modal onClose={() => {}} ariaLabel="test">
        <p>body</p>
      </Modal>,
    );
    expect(document.activeElement).toBe(getByRole('dialog'));

    unmount();
    expect(document.activeElement).toBe(outside);
    outside.remove();
  });

  it('wraps Tab focus from the last focusable back to the first (focus trap)', () => {
    const { getByText, getByRole } = render(
      <Modal onClose={() => {}} ariaLabel="test">
        <button type="button">first</button>
        <button type="button">last</button>
      </Modal>,
    );
    const first = getByText('first');
    const last = getByText('last');

    last.focus();
    fireEvent.keyDown(getByRole('dialog'), { key: 'Tab' });
    expect(document.activeElement).toBe(first);

    first.focus();
    fireEvent.keyDown(getByRole('dialog'), { key: 'Tab', shiftKey: true });
    expect(document.activeElement).toBe(last);
  });
});

describe('Modal dirty-check (issue #730)', () => {
  it('backdrop click on the dimmer with dirty=true shows the discard banner and does NOT close', () => {
    const onClose = vi.fn();
    const { container, getByTestId, queryByTestId } = render(
      <Modal onClose={onClose} dirty ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    expect(queryByTestId('modal-discard-banner')).toBeNull();
    // Click the visible dimmer div (the wrapper's first child). Real users
    // never click the invisible outer wrapper — the earlier
    // `e.target !== e.currentTarget` guard rejected every real backdrop
    // click and the test that clicked the wrapper gave false green.
    const dimmer = (container.firstElementChild as HTMLElement).firstElementChild as HTMLElement;
    fireEvent.click(dimmer);
    expect(onClose).not.toHaveBeenCalled();
    getByTestId('modal-discard-banner');
  });

  it('Escape with dirty=true shows the discard banner and does NOT close', () => {
    const onClose = vi.fn();
    const { getByTestId, queryByTestId } = render(
      <Modal onClose={onClose} dirty ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    expect(queryByTestId('modal-discard-banner')).toBeNull();
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onClose).not.toHaveBeenCalled();
    getByTestId('modal-discard-banner');
  });

  it('backdrop click on the dimmer with dirty=false still closes (existing behaviour preserved)', () => {
    const onClose = vi.fn();
    const { container } = render(
      <Modal onClose={onClose} ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    const dimmer = (container.firstElementChild as HTMLElement).firstElementChild as HTMLElement;
    fireEvent.click(dimmer);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('Escape with dirty=false still closes (existing behaviour preserved)', () => {
    const onClose = vi.fn();
    render(
      <Modal onClose={onClose} ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('clicking Discard calls onClose and removes the banner', () => {
    const onClose = vi.fn();
    const { container, getByTestId, queryByTestId } = render(
      <Modal onClose={onClose} dirty ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    const dimmer = (container.firstElementChild as HTMLElement).firstElementChild as HTMLElement;
    fireEvent.click(dimmer);
    getByTestId('modal-discard-banner');
    fireEvent.click(getByTestId('modal-discard-confirm'));
    expect(onClose).toHaveBeenCalledTimes(1);
    expect(queryByTestId('modal-discard-banner')).toBeNull();
  });

  it('clicking Keep editing hides the banner and restores focus to the field the user was in', async () => {
    // The focus-restore is a requestAnimationFrame-deferred side effect, so
    // we wait one frame before asserting. Real browsers fire mousedown on
    // backdrop click BEFORE the focus moves; we need to mirror that here
    // so the wrapper's onMouseDown handler can capture document.activeElement
    // before the click moves it.
    const onClose = vi.fn();
    const { container, getByTestId, getByDisplayValue } = render(
      <Modal onClose={onClose} dirty ariaLabel="t">
        <input data-testid="typed" defaultValue="half typed" />
      </Modal>,
    );
    // The user is focused in the input (simulating having typed in it).
    const input = getByDisplayValue('half typed') as HTMLInputElement;
    input.focus();
    expect(document.activeElement).toBe(input);

    // Backdrop click on the dimmer — fire mousedown (captures the focused
    // input) and click (sets the banner) separately. fireEvent.click alone
    // would skip the mousedown phase and lastFocusRef would never be set.
    const dimmer = (container.firstElementChild as HTMLElement).firstElementChild as HTMLElement;
    fireEvent.mouseDown(dimmer);
    fireEvent.click(dimmer);
    expect(onClose).not.toHaveBeenCalled();
    getByTestId('modal-discard-banner');

    // Keep editing — banner gone, focus is back on the input.
    fireEvent.click(getByTestId('modal-discard-cancel'));
    await new Promise(requestAnimationFrame);
    expect(document.activeElement).toBe(input);
    expect(onClose).not.toHaveBeenCalled();
  });

  it('after the banner is open, another Escape falls through to onClose (banner is the safeguard, not a lock)', () => {
    // The issue specifies Escape as the close trigger with the confirm as
    // safeguard. Once the banner is already up, the next Escape is the user
    // confirming they want to abandon — so it closes. (A second confirm step
    // would be annoying and contradicts how every OS dialog handles Escape.)
    const onClose = vi.fn();
    const { getByTestId } = render(
      <Modal onClose={onClose} dirty ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    fireEvent.keyDown(window, { key: 'Escape' });
    getByTestId('modal-discard-banner');
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('uses the custom dirtyMessage when provided', () => {
    const onClose = vi.fn();
    const { container, getByText } = render(
      <Modal onClose={onClose} dirty dirtyMessage="Throw away your edit?" ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    const dimmer = (container.firstElementChild as HTMLElement).firstElementChild as HTMLElement;
    fireEvent.click(dimmer);
    getByText('Throw away your edit?');
  });

  it('dirty toggled after mount is still seen by the once-armed Escape handler (regression: stale closure)', () => {
    // The Escape listener is registered with `useEffect([], ...)` — without a
    // `dirtyRef` mirror, the captured `dirty` would be the mount-time value
    // (false here), and the second Escape would close even though the parent
    // has flipped dirty=true via rerender. With the mirror, the new value
    // reaches the listener and the banner shows.
    const onClose = vi.fn();
    const { rerender, getByTestId, queryByTestId } = render(
      <Modal onClose={onClose} ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    expect(queryByTestId('modal-discard-banner')).toBeNull();
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);

    onClose.mockClear();
    rerender(
      <Modal onClose={onClose} dirty ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onClose).not.toHaveBeenCalled();
    getByTestId('modal-discard-banner');
  });

  it('banner auto-dismisses when dirty flips false (regression: stays up after save)', () => {
    // User types a draft, hits Escape (banner appears), then saves — the
    // save succeeds, isDirty flips to false, dirty prop becomes false, and
    // the banner should auto-dismiss. Without the auto-dismiss effect the
    // user is stranded behind a "Discard changes?" prompt for content
    // they just saved.
    const onClose = vi.fn();
    const { rerender, getByTestId, queryByTestId } = render(
      <Modal onClose={onClose} dirty ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    fireEvent.keyDown(window, { key: 'Escape' });
    getByTestId('modal-discard-banner');

    rerender(
      <Modal onClose={onClose} dirty={false} ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    expect(queryByTestId('modal-discard-banner')).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
  });

  it('moves focus to the Keep-editing button when the banner appears (WAI-ARIA APG alertdialog)', () => {
    const onClose = vi.fn();
    const { getByTestId } = render(
      <Modal onClose={onClose} dirty ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(document.activeElement).toBe(getByTestId('modal-discard-cancel'));
  });
});
