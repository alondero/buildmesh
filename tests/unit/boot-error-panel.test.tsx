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
 *
 * Issue #1525 — the panel must name the *resolved absolute* log path, not a
 * bare filename. It used to promise details were in "buildmesh.log" with no
 * path, so a user reporting a boot failure had no way to find the file. The
 * panel now asks the backend (`getDiagnosticPaths`) for the location the
 * startup bootstrap resolved before the database opened. Three cases matter:
 * it resolves, it rejects (the backend may be what failed), and the user can
 * still read the raw error either way.
 */

import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { BootErrorPanel } from '../../src/components/BootErrorPanel/BootErrorPanel';

const tauriMocks = vi.hoisted(() => ({
  getDiagnosticPaths: vi.fn(),
}));

vi.mock('../../src/lib/tauri', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../src/lib/tauri')>()),
  getDiagnosticPaths: tauriMocks.getDiagnosticPaths,
}));

const RESOLVED = {
  profile_dir: 'C:\\Users\\test\\AppData\\Roaming\\com.alond.buildmesh',
  log_dir: 'C:\\Users\\test\\AppData\\Roaming\\com.alond.buildmesh\\logs',
  main_log: 'C:\\Users\\test\\AppData\\Roaming\\com.alond.buildmesh\\logs\\buildmesh.log',
  panic_log: 'C:\\Users\\test\\AppData\\Roaming\\com.alond.buildmesh\\logs\\panic.log',
};

beforeEach(() => {
  tauriMocks.getDiagnosticPaths.mockReset();
  tauriMocks.getDiagnosticPaths.mockResolvedValue(RESOLVED);
});

/**
 * Wait for the panel's log-path lookup to land before asserting.
 *
 * `BootErrorPanel` resolves `getDiagnosticPaths` in an effect, so every render
 * schedules a state update. A test that returns without letting that promise
 * settle leaves React updating a component outside `act(...)`, which is the
 * "not wrapped in act(...)" warning — intermittently, depending on timing.
 *
 * The wait is on the *rendered result*, not on the mock having been called:
 * the effect invokes the command synchronously, so waiting on the call
 * returns before React has processed the resolved promise and the helper would
 * be a no-op. These tests use the resolving mock, so the path element appearing
 * is the observable proof the update has been applied.
 */
async function settle() {
  await screen.findByTestId('boot-log-path');
}

describe('BootErrorPanel (issue #1250: surface init failures)', () => {
  it('renders the formatted error in a readable description', async () => {
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
    await settle();
  });

  it('renders the raw error in a copyable <pre> block', async () => {
    // Mirrors ErrorBoundary: a fixed-height scrollable block with the raw
    // text, so the user can copy/paste it into a bug report without
    // having to dig out devtools.
    const { container } = render(
      <BootErrorPanel error="connection refused: 127.0.0.1:1992" onRetry={vi.fn()} />,
    );
    const pre = container.querySelector('pre');
    expect(pre).toBeTruthy();
    expect(pre!.textContent).toContain('connection refused: 127.0.0.1:1992');
    await settle();
  });

  it('exposes a Retry button that calls onRetry when clicked', async () => {
    const onRetry = vi.fn();
    const user = userEvent.setup();
    render(<BootErrorPanel error="boom" onRetry={onRetry} />);

    await user.click(screen.getByRole('button', { name: /^retry$/i }));

    expect(onRetry).toHaveBeenCalledTimes(1);
    await settle();
  });

  it('disables the Retry button while busy so overlapping re-inits cannot be triggered', async () => {
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
    await settle();
  });

  it('re-enables the Retry button when busy flips back off', async () => {
    const { rerender } = render(
      <BootErrorPanel error="boom" onRetry={vi.fn()} busy />,
    );
    await settle();
    expect(
      screen.getByRole('button', { name: /^retry$/i }).hasAttribute('disabled'),
    ).toBe(true);

    rerender(<BootErrorPanel error="boom" onRetry={vi.fn()} busy={false} />);
    expect(
      screen.getByRole('button', { name: /^retry$/i }).hasAttribute('disabled'),
    ).toBe(false);
  });

  it('points the user at the resolved absolute log file, not a bare filename', async () => {
    render(<BootErrorPanel error="boom" onRetry={vi.fn()} />);

    // Issue #1525: the promise must be actionable. "buildmesh.log" alone
    // leaves the user hunting for the profile directory.
    const logPath = await screen.findByTestId('boot-log-path');
    expect(logPath.textContent).toBe(RESOLVED.main_log);
    expect(logPath.textContent).toContain('buildmesh.log');
    expect(screen.getByText(/written to/i)).toBeTruthy();
  });

  it('asks the backend once for the log location rather than guessing', async () => {
    const { rerender } = render(<BootErrorPanel error="boom" onRetry={vi.fn()} />);
    await screen.findByTestId('boot-log-path');
    // A re-render (busy toggling while a retry is in flight) must not re-ask:
    // the path cannot change while the process lives.
    rerender(<BootErrorPanel error="boom" onRetry={vi.fn()} busy />);
    await waitFor(() => expect(tauriMocks.getDiagnosticPaths).toHaveBeenCalledTimes(1));
  });

  it('still shows the raw error when the log-path lookup itself fails', async () => {
    // The backend may be the very thing that failed, in which case this
    // rejects. The panel must degrade to "no path shown", not to a second
    // error surface — the raw error is already on screen and copyable.
    tauriMocks.getDiagnosticPaths.mockRejectedValue(new Error('not available'));

    render(<BootErrorPanel error="database is locked" onRetry={vi.fn()} />);

    // Wait for the rejection to be handled, not merely for the call to be
    // made: the state update that clears the path is what would otherwise land
    // after the test body finished.
    await waitFor(() =>
      expect(tauriMocks.getDiagnosticPaths).toHaveBeenCalledTimes(1),
    );
    await waitFor(() => expect(screen.queryByTestId('boot-log-path')).toBeNull());
    expect(screen.getByText('database is locked')).toBeTruthy();
    // And it must not make a *second* promise about a log it cannot locate.
    expect(screen.queryByText(/written to/i)).toBeNull();
  });

  it('settles only after the lookup result is actually rendered', async () => {
    // Guards the helper itself. An earlier `settle()` waited on the mock having
    // been *called*, which the effect does synchronously — so it returned
    // before React applied the update and drained nothing. If this test passes,
    // `settle()` provably waits for the rendered result, not the call.
    render(<BootErrorPanel error="boom" onRetry={vi.fn()} />);
    await settle();
    expect(screen.getByTestId('boot-log-path').textContent).toBe(
      RESOLVED.main_log,
    );
  });

  it('marks the panel as role="alert" so screen readers announce it', async () => {
    // aria-live defaults to "assertive" via role=alert — the user *must*
    // be told the app failed to boot, not discover it by absence of UI.
    const { container } = render(
      <BootErrorPanel error="boom" onRetry={vi.fn()} />,
    );
    expect(container.querySelector('[role="alert"]')).toBeTruthy();
    await settle();
  });
});