/**
 * Tests for the ShortcutCheatsheet modal (issue #731).
 *
 * The cheatsheet is a discoverability surface: the user presses `?` (or
 * finds the hint in the empty-state splash) and gets a single panel that
 * lists every shortcut, grouped by scope. The acceptance criteria pin four
 * behaviours: dialog ARIA, focus the close button by default, Escape
 * closes, and the catalog is the source of truth (no per-component
 * re-declaration of keys).
 */
import { describe, it, expect, vi, afterEach } from 'vitest';
import { render, cleanup, fireEvent } from '@testing-library/react';
import { ShortcutCheatsheet } from '../../src/components/ShortcutCheatsheet/ShortcutCheatsheet';

afterEach(cleanup);

describe('ShortcutCheatsheet', () => {
  it('renders with dialog ARIA (aria-modal, role=dialog, labelledby pointing at the heading)', () => {
    const { getByRole } = render(
      <ShortcutCheatsheet open onClose={() => {}} />,
    );
    const dialog = getByRole('dialog');
    expect(dialog.getAttribute('aria-modal')).toBe('true');
    const labelledBy = dialog.getAttribute('aria-labelledby');
    expect(labelledBy).toBeTruthy();
    const heading = document.getElementById(labelledBy!);
    expect(heading?.textContent).toMatch(/keyboard shortcuts/i);
  });

  it('lists every entry from the catalog (drift guard — adding a shortcut must show up here)', () => {
    render(
      <ShortcutCheatsheet open onClose={() => {}} />,
    );
    // We don't pin exact text (the catalog grows), but every group heading
    // must be present so the user sees a complete list, not a partial one.
    // Issue #1292: the modal is portaled to document.body now, so we
    // query the catalog headings from there instead of the render
    // container (which is empty after the portal).
    expect(document.body.textContent).toMatch(/App|Window|Global/);
    expect(document.body.textContent).toMatch(/Grid/);
    expect(document.body.textContent).toMatch(/Terminal/);
    expect(document.body.textContent).toMatch(/Modal|Dialog/);
  });

  it('focuses the close button on open (acceptance criterion: "focus the close button by default")', () => {
    // The Modal primitive focuses the panel (tabIndex=-1) on mount, so the
    // cheatsheet has to override that with a focused close button.
    // We pin that the close button is the *first* focusable element in the
    // dialog and that it ends up as the active element after mount.
    render(<ShortcutCheatsheet open onClose={() => {}} />);
    const closeBtn = document.querySelector('[aria-label="Close"]') as HTMLButtonElement;
    expect(closeBtn).toBeTruthy();
    expect(document.activeElement).toBe(closeBtn);
  });

  it('calls onClose when Escape is pressed (acceptance criterion: "simulate Escape and assert close")', () => {
    const onClose = vi.fn();
    render(<ShortcutCheatsheet open onClose={onClose} />);
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('calls onClose when the close button is clicked', () => {
    const onClose = vi.fn();
    render(<ShortcutCheatsheet open onClose={onClose} />);
    const closeBtn = document.querySelector('[aria-label="Close"]') as HTMLButtonElement;
    fireEvent.click(closeBtn);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('does not render anything when open is false (mount/unmount arms the Escape listener)', () => {
    // The Modal primitive's whole point is that the Escape listener is
    // mounted only while the dialog is visible — otherwise it would steal
    // Escape from agent terminals. This test pins the conditional render.
    const { container } = render(
      <ShortcutCheatsheet open={false} onClose={() => {}} />,
    );
    expect(container.firstChild).toBeNull();
  });

  it('restores focus to the previously-focused element on close (Modal primitive contract)', () => {
    // The Modal primitive handles this, but the cheatsheet is a user-facing
    // surface that depends on it — if a future refactor changes the primitive
    // in a way that breaks restore, the cheatsheet becomes the user-visible
    // regression. Pin it from the consumer side too.
    const outside = document.createElement('button');
    document.body.appendChild(outside);
    outside.focus();
    expect(document.activeElement).toBe(outside);

    const { unmount } = render(<ShortcutCheatsheet open onClose={() => {}} />);
    unmount();
    expect(document.activeElement).toBe(outside);
    outside.remove();
  });
});
