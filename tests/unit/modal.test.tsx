/**
 * Tests for the shared <Modal> primitive (design-system pass).
 *
 * Every dialog in the app previously hand-rolled its own shell and they had
 * drifted: only one of four handled Escape, none set dialog ARIA, none moved
 * focus. These tests pin the contract the four retrofitted modals now rely on.
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
    const overlay = container.firstElementChild as HTMLElement;
    fireEvent.click(overlay);
    expect(onClose).toHaveBeenCalledTimes(1);

    onClose.mockClear();
    rerender(
      <Modal onClose={onClose} ariaLabel="test" closeOnBackdrop={false}>
        <p>body</p>
      </Modal>,
    );
    fireEvent.click(container.firstElementChild as HTMLElement);
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
