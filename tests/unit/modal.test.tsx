/**
 * Tests for the shared <Modal> primitive (design-system pass).
 *
 * Every dialog in the app previously hand-rolled its own shell and they had
 * drifted: only one of four handled Escape, none set dialog ARIA, none moved
 * focus. These tests pin the contract the four retrofitted modals now rely on.
 *
 * The dirty-check tests at the bottom pin the issue #730 contract: a half-typed
 * form is not silently destroyed by a stray backdrop click or Escape.
 *
 * Issue #1292: the modal is portaled to `document.body`. Tests that need to
 * click the visible dimmer walk from the dialog role (still present in the
 * same DOM tree, just relocated) up to its parent — the fixed wrapper — and
 * take its first child, matching the structure
 * `<div fixed> > <div dimmer> + <div role="dialog">`.
 */
import { describe, it, expect, vi, afterEach } from 'vitest';
import { useRef } from 'react';
import { render, cleanup, fireEvent, screen } from '@testing-library/react';
import { Modal } from '../../src/components/shared/Modal';
import { ConfirmDialog } from '../../src/components/ConfirmDialog/ConfirmDialog';

afterEach(cleanup);

/** Issue #1292: with the portal, the wrapper/dimmer live in `document.body`,
 *  not in the render container. Walk from the dialog role upward to the
 *  fixed wrapper, then take its first child (the dimmer). The panel is
 *  always present (it renders the dialog role unconditionally), so this
 *  works for both dirty and non-dirty modals. */
function getBackdrop(): HTMLElement {
  const wrapper = screen.getByRole('dialog').parentElement as HTMLElement;
  return wrapper.firstElementChild as HTMLElement;
}

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
    const { rerender } = render(
      <Modal onClose={onClose} ariaLabel="test">
        <p>body</p>
      </Modal>,
    );
    // The visible dimmer is the wrapper's first child (an absolute-positioned
    // <div className="absolute inset-0 bg-bg-base/70 backdrop-blur-sm" />).
    // A real user clicks the dimmer, not the wrapper. Earlier versions of
    // this test clicked the wrapper and gave false green coverage when the
    // handler had a `e.target !== e.currentTarget` guard (code-review catch).
    // Issue #1292: getBackdrop() walks from document.body now that the
    // modal is portaled — the helper at the top of this file explains.
    const dimmer = getBackdrop();
    fireEvent.click(dimmer);
    expect(onClose).toHaveBeenCalledTimes(1);

    onClose.mockClear();
    rerender(
      <Modal onClose={onClose} ariaLabel="test" closeOnBackdrop={false}>
        <p>body</p>
      </Modal>,
    );
    fireEvent.click(getBackdrop());
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

describe('Modal defaultFocusRef (issue #748)', () => {
  // Consumer-supplied ref pattern. The ref points at an arbitrary focusable
  // child (here: a CTA button rendered alongside other content). The modal
  // focuses that child instead of the panel — the rationale: the consumer
  // picks the focus target that matches its acceptance criteria (close
  // button, primary action, first form field, etc.) without coupling to
  // the modal's chrome.
  it('focuses the element referenced by defaultFocusRef instead of the panel', () => {
    function Harness() {
      const ctaRef = useRef<HTMLButtonElement>(null);
      return (
        <Modal onClose={() => {}} ariaLabel="t" defaultFocusRef={ctaRef}>
          <p>body</p>
          <button type="button" ref={ctaRef}>primary action</button>
        </Modal>
      );
    }
    const { getByText, getByRole } = render(<Harness />);
    const cta = getByText('primary action') as HTMLButtonElement;
    expect(document.activeElement).toBe(cta);
    // The panel must NOT have been focused instead — pinning the contract
    // that defaultFocusRef actually overrides, not augments.
    expect(document.activeElement).not.toBe(getByRole('dialog'));
  });

  it('falls back to the panel when defaultFocusRef points at nothing', () => {
    // Consumer wires the prop but the ref hasn't attached yet (e.g. the
    // element is conditionally rendered). The modal must still focus
    // somewhere — falling back to the panel is the existing behaviour
    // and the safest default (panel itself is focusable via tabIndex=-1).
    const emptyRef = { current: null };
    const { getByRole } = render(
      <Modal onClose={() => {}} ariaLabel="t" defaultFocusRef={emptyRef}>
        <p>body</p>
      </Modal>,
    );
    expect(document.activeElement).toBe(getByRole('dialog'));
  });
});

describe('Modal dirty-check (issue #730)', () => {
  it('backdrop click on the dimmer with dirty=true shows the discard banner and does NOT close', () => {
    const onClose = vi.fn();
    const { getByTestId, queryByTestId } = render(
      <Modal onClose={onClose} dirty ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    expect(queryByTestId('modal-discard-banner')).toBeNull();
    // Click the visible dimmer div (the wrapper's first child). Real users
    // never click the invisible outer wrapper — the earlier
    // `e.target !== e.currentTarget` guard rejected every real backdrop
    // click and the test that clicked the wrapper gave false green.
    const dimmer = getBackdrop();
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
    render(
      <Modal onClose={onClose} ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    const dimmer = getBackdrop();
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
    const { getByTestId, queryByTestId } = render(
      <Modal onClose={onClose} dirty ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    const dimmer = getBackdrop();
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
    const { getByTestId, getByDisplayValue } = render(
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
    const dimmer = getBackdrop();
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

  it('after the banner is open, a second Escape dismisses the banner and does NOT discard (issue #808)', () => {
    // A "discard unsaved changes?" prompt must map Escape to the SAFE option
    // (Keep editing), matching how OS dialogs treat Escape on a confirmation.
    // Reflexively pressing Escape to dismiss the prompt must never destroy the
    // user's work — only the explicit Discard button closes.
    const onClose = vi.fn();
    const { getByTestId, queryByTestId } = render(
      <Modal onClose={onClose} dirty ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    fireEvent.keyDown(window, { key: 'Escape' });
    getByTestId('modal-discard-banner');
    expect(onClose).not.toHaveBeenCalled();
    // Second Escape — banner gone, modal still open.
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(queryByTestId('modal-discard-banner')).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
  });

  it('after the banner is open, a backdrop click dismisses the banner and does NOT discard (issue #808)', () => {
    const onClose = vi.fn();
    const { getByTestId, queryByTestId } = render(
      <Modal onClose={onClose} dirty ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    const dimmer = getBackdrop();
    fireEvent.click(dimmer);
    getByTestId('modal-discard-banner');
    expect(onClose).not.toHaveBeenCalled();
    // Second backdrop click — banner gone, modal still open.
    fireEvent.click(dimmer);
    expect(queryByTestId('modal-discard-banner')).toBeNull();
    expect(onClose).not.toHaveBeenCalled();
  });

  it('the explicit Discard button is the only path that closes a dirty modal', () => {
    const onClose = vi.fn();
    const { getByTestId } = render(
      <Modal onClose={onClose} dirty ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    fireEvent.keyDown(window, { key: 'Escape' });
    fireEvent.click(getByTestId('modal-discard-confirm'));
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('uses the custom dirtyMessage when provided', () => {
    const onClose = vi.fn();
    const { getByText } = render(
      <Modal onClose={onClose} dirty dirtyMessage="Throw away your edit?" ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    const dimmer = getBackdrop();
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

describe('Modal portals to document.body (issue #1292)', () => {
  // Follow-up of #1290 / PR #1290. The previous fix portaled the sidebar's
  // NodeItem and MeshItem context menus so `position:fixed` resolves against
  // the viewport instead of a `filter`/`transform` containing block. The
  // running-node Regenerate `<ConfirmDialog>` is still nested inside the
  // NodeItem row, which applies `hover:brightness-125` (`filter`), so it
  // inherits the same bug. Patching the call sites one by one would chain
  // through ConfirmDialog → MeshPropertiesTab → WorktreeManagerTab → …
  // Doing it once inside `Modal` covers every consumer.
  //
  // The bug was: `position: fixed` resolves against the nearest ancestor
  // that creates a containing block. CSS properties that do this include
  // `filter`, `transform`, `opacity`, `backdrop-filter`, `perspective`,
  // `contain: paint | layout | strict | content`, and `will-change` of any
  // of those. With the wrapper nested under any of them, `fixed inset-0`
  // shrinks to that ancestor's box and the dialog stops covering the
  // window.

  it('renders its DOM tree under document.body, not under the call site', () => {
    // Render inside an element we can identify. Without the portal, the
    // wrapper/dimmer/panel would all live under `[data-anchor]`.
    const { getByRole } = render(
      <div data-anchor>
        <Modal onClose={() => {}} ariaLabel="t">
          <p>body</p>
        </Modal>
      </div>,
    );
    const dialog = getByRole('dialog');
    // No ancestor in the dialog's parent chain carries the anchor.
    let node: HTMLElement | null = dialog;
    while (node && node !== document.body) {
      expect(node.getAttribute('data-anchor')).toBeNull();
      node = node.parentElement;
    }
    // The chain reaches `document.body` — the portal target.
    expect(node).toBe(document.body);
  });

  it('avoids `filter` / `transform` ancestors so `position:fixed inset-0` covers the viewport', () => {
    // The exact failure mode from the issue: the NodeItem row uses
    // `hover:brightness-125` (Tailwind's `filter: brightness(1.25)`) and
    // the parent MeshItem uses dnd-kit's `transform`. `filter` and
    // `transform` both create a containing block, so before the portal
    // `fixed inset-0` resolved against the row, not the window.
    //
    // We force the filter+transform values via inline style so the test
    // runs in jsdom (Tailwind's hover variant doesn't apply without a
    // real DOM hover). The assertion is the same either way: the
    // dialog's ancestor chain must not cross either property.
    const { getByRole } = render(
      <div style={{ filter: 'brightness(1.25)', transform: 'translate(10px, 10px)' }}>
        <Modal onClose={() => {}} ariaLabel="t">
          <p>body</p>
        </Modal>
      </div>,
    );
    const dialog = getByRole('dialog');
    let node: HTMLElement | null = dialog;
    while (node && node !== document.body) {
      // jsdom returns the literal inline style; in a real browser the
      // computed style would also report a non-`none` filter/transform
      // on the same element. Either form would force a containing
      // block.
      expect(node.style.filter).toBe('');
      expect(node.style.transform).toBe('');
      node = node.parentElement;
    }
    expect(node).toBe(document.body);
  });

  it('the portal target is `document.body` exactly — not a custom container', () => {
    // Belt-and-braces pin: if someone later swaps the portal target for
    // a `<div id="modal-root">` (a common anti-pattern — it re-introduces
    // containing-block issues if the root ever grows a `transform`), this
    // test fails loudly.
    const { getByRole } = render(
      <Modal onClose={() => {}} ariaLabel="t">
        <p>body</p>
      </Modal>,
    );
    expect(getByRole('dialog').parentElement?.parentElement).toBe(document.body);
  });
});

describe('ConfirmDialog inherits the Modal portal (issue #1292)', () => {
  // ConfirmDialog is a thin wrapper around Modal — the fix at the Modal
  // boundary covers it for free, but we pin the production case (which
  // came in through the bug report) so a future refactor can't quietly
  // re-introduce a wrapper that breaks the portal.

  it('the running-node Regenerate dialog overlays document.body, not the NodeItem row', () => {
    // Mimic the production layout: an Agent Node row applies
    // `hover:brightness-125` (filter) and is itself a child of a
    // MeshItem that uses dnd-kit's `transform`. The `data-session-item`
    // attribute is the production marker (NodeItem.tsx) so the test
    // ties back to the real failure surface, not an abstract wrapper.
    function Harness() {
      return (
        <div style={{ filter: 'brightness(1.25)', transform: 'translate(0, 0)' }} data-session-item="42">
          <ConfirmDialog
            title="Regenerate this node?"
            message="Agent is currently working."
            confirmLabel="Regenerate"
            onConfirm={() => {}}
            onCancel={() => {}}
          />
        </div>
      );
    }
    render(<Harness />);

    const dialog = screen.getByRole('dialog');
    // Walk up from the dialog and assert we never cross the
    // `data-session-item` ancestor (the row that triggered the bug).
    let node: HTMLElement | null = dialog;
    while (node && node !== document.body) {
      expect(node.getAttribute('data-session-item')).toBeNull();
      node = node.parentElement;
    }
    expect(node).toBe(document.body);
  });

  it('ConfirmDialog still closes on backdrop click after the portal move', () => {
    // Behaviour preserved across the portal change: the same backdrop
    // click path that worked before still works after we relocated the
    // DOM. Clicking the dimmer invokes `onCancel`.
    const onCancel = vi.fn();
    render(
      <ConfirmDialog
        title="Delete?"
        message="This cannot be undone."
        confirmLabel="Delete"
        onConfirm={() => {}}
        onCancel={onCancel}
      />,
    );
    fireEvent.click(getBackdrop());
    expect(onCancel).toHaveBeenCalledTimes(1);
  });
});
