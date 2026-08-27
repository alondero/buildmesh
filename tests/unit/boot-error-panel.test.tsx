/**
 * Tests for `<BootErrorPanel>` — the full-panel error UI shown when one of
 * the IPC calls in `App.init()` rejects (issue #1250).
 *
 * Before this fix, an init failure left the pulsing splash on screen
 * forever with no user-facing signal — the error went to `console.error`
 * and was otherwise discarded. The panel must:
 *   - explain what went wrong (the formatted error string)
 *   - surface the raw error so the user can copy/paste it
 *   - offer a Retry button that re-runs `App.init()`
 *   - disable Retry while a re-init is in flight (so a panicking backend
 *     doesn't get hammered with overlapping calls)
 *
 * Reuses the same vocabulary as `ErrorBoundary` (the canonical full-panel
 * error UI in this codebase): centered `bg-bg-base` wrapper,
 * `bg-bg-surface border-status-error/50` card, ⚠ icon, secondary
 * description, raw error in a `<pre>`.
 */

import { describe, it, expect, vi } from 'vitest';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { BootErrorPanel } from '../../src/components/BootErrorPanel/BootErrorPanel';

describe('BootErrorPanel (issue #1250: surface init failures)', () => {
  it('renders the formatted error in a readable description', () => {
    const { container } = render(
      <BootErrorPanel error="database is locked" onRetry={vi.fn()} />,
    );
    // Headline distinguishes this from ErrorBoundary's "render error" —
    // the user needs to know this is a *boot* failure, not a crash.
    expect(screen.getByRole('heading', { name: /couldn'?t initialize buildmesh/i })).toBeTruthy();
    // The error message lives in the <pre> block (same place ErrorBoundary
    // puts it). The descriptive paragraph above mentions the log file but
    // doesn't repeat the raw message — keeps the copy dense.
    const pre = container.querySelector('pre');
    expect(pre).toBeTruthy();
    expect(pre!.textContent).toContain('database is locked');
  });

  it('renders the raw error in a copyable <pre> block', () => {
    // Mirrors ErrorBoundary: a fixed-height scrollable block with the raw
    // text, so the user can copy/paste it into a bug report without
    // having to dig out devtools.
    const { container } = render(
      <BootErrorPanel error="connection refused: 127.0.0.1:1992" onRetry={vi.fn()} />,
    );
    const pre = container.querySelector('pre');
    expect(pre).toBeTruthy();
    expect(pre!.textContent).toContain('connection refused: 127.0.0.1:1992');
  });

  it('exposes a Retry button that calls onRetry when clicked', async () => {
    const onRetry = vi.fn();
    const user = userEvent.setup();
    render(<BootErrorPanel error="boom" onRetry={onRetry} />);

    await user.click(screen.getByRole('button', { name: /^retry$/i }));

    expect(onRetry).toHaveBeenCalledTimes(1);
  });

  it('disables the Retry button while busy so overlapping re-inits cannot be triggered', () => {
    // Race regression: without this gate, a panicking backend would get
    // a second IPC burst before the first batch resolved. Same defensive
    // pattern as AccountCard's busy guard on in-flight remove.
    const onRetry = vi.fn();
    render(<BootErrorPanel error="boom" onRetry={onRetry} busy />);

    const button = screen.getByRole('button', { name: /^retry$/i });
    // We use hasAttribute rather than jest-dom's toBeDisabled because
    // the repo's vitest setup doesn't extend jest-dom — the existing
    // tests verify disabled state via direct attribute checks.
    expect(button.hasAttribute('disabled')).toBe(true);

    // Sanity-check: clicking a disabled native button does not fire
    // onClick at all (browser semantics), so onRetry stays at 0.
    button.click();
    expect(onRetry).not.toHaveBeenCalled();
  });

  it('re-enables the Retry button when busy flips back off', () => {
    const { rerender } = render(
      <BootErrorPanel error="boom" onRetry={vi.fn()} busy />,
    );
    expect(
      screen.getByRole('button', { name: /^retry$/i }).hasAttribute('disabled'),
    ).toBe(true);

    rerender(<BootErrorPanel error="boom" onRetry={vi.fn()} busy={false} />);
    expect(
      screen.getByRole('button', { name: /^retry$/i }).hasAttribute('disabled'),
    ).toBe(false);
  });

  it('points the user at the log file so they have somewhere to look for the full trace', () => {
    render(<BootErrorPanel error="boom" onRetry={vi.fn()} />);
    // The dev-facing recovery path: open buildmesh.log next to the exe.
    expect(screen.getByText(/buildmesh\.log/)).toBeTruthy();
  });

  it('marks the panel as role="alert" so screen readers announce it', () => {
    // aria-live defaults to "assertive" via role=alert — the user *must*
    // be told the app failed to boot, not discover it by absence of UI.
    const { container } = render(
      <BootErrorPanel error="boom" onRetry={vi.fn()} />,
    );
    expect(container.querySelector('[role="alert"]')).toBeTruthy();
  });
});