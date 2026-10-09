/**
 * Blocked worktree cleanup dialog (issue #2139).
 *
 * The dialog is the recovery surface for a worktree removal that could not
 * complete. What matters is that it shows the *evidence* the backend persisted
 * (path, failed step, error, attempts, backoff) rather than a generic warning,
 * that the recoveries are all reachable, and that the destructive one — ending a
 * process — asks first (issue #2139 review round 1).
 */

import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { render, screen, fireEvent, act, within } from '@testing-library/react';
import { BlockedCleanupDialog } from '../../src/components/BlockedCleanupDialog/BlockedCleanupDialog';
import { useBlockedCleanupStore } from '../../src/stores/blockedCleanupStore';
import * as api from '../../src/lib/tauri';
import { useToastStore } from '../../src/stores/toastStore';
import type { PendingWorktreeRemoval } from '../../src/types/generated/PendingWorktreeRemoval';

vi.mock('../../src/lib/tauri', () => ({
  listPendingWorktreeRemovals: vi.fn(),
  retryWorktreeCleanup: vi.fn(),
  dismissWorktreeCleanup: vi.fn(),
  diagnoseWorktreeCleanupBlockers: vi.fn(),
  releaseWorktreeCleanupBlocker: vi.fn(),
}));

const HOLDER = {
  pid: 5212,
  name: 'powershell.exe',
  executable_path: 'C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe',
  reason: 'working-directory',
  detail: 'C:/repo/.claude/worktrees/agent-node/child',
};

function blockedRow(overrides: Partial<PendingWorktreeRemoval> = {}): PendingWorktreeRemoval {
  return {
    worktree_path: 'C:/repo/.claude/worktrees/agent-node',
    node_name: 'agent-node',
    attempt_count: 3,
    last_attempt_at: Date.now(),
    last_operation: 'rename-worktree-to-staging',
    last_error:
      'The process cannot access the file because it is being used by another process. (os error 32)',
    retry_not_before: Date.now() + 60_000,
    notified_error: 'rename-worktree-to-staging: os error 32',
    notified_at: Date.now(),
    ...overrides,
  };
}

function openWith(entries: PendingWorktreeRemoval[]) {
  useBlockedCleanupStore.setState({
    entries,
    isOpen: true,
    loading: false,
    blockers: {},
    actionStatus: {},
    pendingKill: null,
  });
}

/** The one blocked cleanup most tests drive: blocked three times by a process
 *  that holds a child directory, last attempt one minute ago, retry due in a
 *  minute. The clock tests override both timestamps. */
const ROW: PendingWorktreeRemoval = blockedRow({
  last_attempt_at: Date.now() - 60_000,
  retry_not_before: Date.now() + 60_000,
});

describe('BlockedCleanupDialog (#2139)', () => {
  beforeEach(() => {
    useBlockedCleanupStore.setState({ entries: [], isOpen: false, loading: false, blockers: {}, actionStatus: {} });
    const writeText = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    vi.mocked(api.retryWorktreeCleanup).mockReset().mockResolvedValue({ status: 'still-blocked', record: ROW });
    vi.mocked(api.dismissWorktreeCleanup).mockReset().mockResolvedValue(undefined);
    vi.mocked(api.diagnoseWorktreeCleanupBlockers).mockReset().mockResolvedValue([]);
  });

  it('renders nothing while no cleanup is blocked', () => {
    useBlockedCleanupStore.setState({ entries: [], isOpen: false });
    const { container } = render(<BlockedCleanupDialog />);
    // The modal renders through a portal into `document.body`, so the render
    // container stays empty either way — assert the portal content is absent
    // instead of a container check that would pass with the dialog open.
    expect(container.firstChild).toBeNull();
    expect(document.querySelector('[role="dialog"]')).toBeNull();
    expect(screen.queryByText(/Worktree cleanup blocked/)).toBeNull();
  });

  it('shows the node, the full path, the failed step and the OS error', () => {
    openWith([ROW]);
    render(<BlockedCleanupDialog />);

    expect(screen.getByText('agent-node')).toBeTruthy();
    expect(screen.getByText(ROW.worktree_path)).toBeTruthy();
    expect(screen.getByText(/Moving the folder aside to remove it/)).toBeTruthy();
    expect(screen.getByText(/os error 32/)).toBeTruthy();
    expect(screen.getByText('3')).toBeTruthy(); // attempt count is shown
  });

  it('offers copy path, copy diagnostics, diagnose, retry and keep worktree', () => {
    openWith([ROW]);
    render(<BlockedCleanupDialog />);

    expect(screen.getByRole('button', { name: 'Copy path' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Copy diagnostics' })).toBeTruthy();
    expect(screen.getByRole('button', { name: /what is holding it/i })).toBeTruthy();
    expect(screen.getByRole('button', { name: /^Retry$/ })).toBeTruthy();
    expect(screen.getByRole('button', { name: /keep worktree/i })).toBeTruthy();
  });

  it('copies the bare path, not the diagnostics block', async () => {
    openWith([ROW]);
    render(<BlockedCleanupDialog />);

    fireEvent.click(screen.getByRole('button', { name: 'Copy path' }));

    expect(navigator.clipboard.writeText).toHaveBeenCalledTimes(1);
    expect((navigator.clipboard.writeText as unknown as { mock: { calls: string[][] } }).mock.calls[0][0]).toBe(
      ROW.worktree_path,
    );
  });

  it('copies the diagnostics block with the blocker evidence attached', async () => {
    openWith([ROW]);
    useBlockedCleanupStore.setState({
      blockers: {
        [ROW.worktree_path]: [
          {
            pid: 5212,
            name: 'powershell.exe',
            executable_path: 'C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe',
            reason: 'working-directory',
            detail: 'C:/repo/.claude/worktrees/agent-node/child',
          },
        ],
      },
    });
    render(<BlockedCleanupDialog />);

    fireEvent.click(screen.getByRole('button', { name: 'Copy diagnostics' }));

    const call = (navigator.clipboard.writeText as unknown as { mock: { calls: string[][] } }).mock.calls[0][0];
    expect(call).toContain(ROW.worktree_path);
    expect(call).toContain('os error 32');
    expect(call).toContain('powershell.exe (pid 5212)');
  });

  it('retries the cleanup for that path when Retry is pressed', async () => {
    openWith([ROW]);
    render(<BlockedCleanupDialog />);

    fireEvent.click(screen.getByRole('button', { name: /^Retry$/ }));

    await vi.waitFor(() => expect(api.retryWorktreeCleanup).toHaveBeenCalledWith(ROW.worktree_path));
  });

  it('diagnoses processes on request, then asks before ending one', async () => {
    openWith([ROW]);
    vi.mocked(api.diagnoseWorktreeCleanupBlockers).mockResolvedValue([HOLDER]);
    vi.mocked(api.releaseWorktreeCleanupBlocker).mockReset().mockResolvedValue(undefined);
    render(<BlockedCleanupDialog />);

    fireEvent.click(screen.getByRole('button', { name: /what is holding it/i }));
    await vi.waitFor(() => expect(screen.getByText(/powershell\.exe \(pid 5212\)/)).toBeTruthy());

    // Ending a process is destructive, so the click must not reach the backend
    // directly (issue #2139 review round 1).
    fireEvent.click(screen.getByRole('button', { name: /end process/i }));
    expect(api.releaseWorktreeCleanupBlocker).not.toHaveBeenCalled();

    // The confirmation names the process and the tree kill it implies.
    const dialogs = document.querySelectorAll('[role="dialog"]');
    const confirm = dialogs[dialogs.length - 1];
    expect(confirm.textContent).toContain('powershell.exe');
    expect(confirm.textContent).toContain('5212');
    expect(confirm.textContent).toMatch(/every process it started/);

    fireEvent.click(within(confirm as HTMLElement).getByRole('button', { name: /end process/i }));
    await vi.waitFor(() =>
      expect(api.releaseWorktreeCleanupBlocker).toHaveBeenCalledWith(ROW.worktree_path, 5212),
    );
  });

  it('does not end the process when the confirmation is cancelled', async () => {
    openWith([ROW]);
    vi.mocked(api.diagnoseWorktreeCleanupBlockers).mockResolvedValue([HOLDER]);
    render(<BlockedCleanupDialog />);

    fireEvent.click(screen.getByRole('button', { name: /what is holding it/i }));
    await vi.waitFor(() => expect(screen.getByText(/powershell\.exe \(pid 5212\)/)).toBeTruthy());
    fireEvent.click(screen.getByRole('button', { name: /end process/i }));

    expect(useBlockedCleanupStore.getState().pendingKill).not.toBeNull();
    fireEvent.click(screen.getByRole('button', { name: /^Cancel$/ }));

    expect(useBlockedCleanupStore.getState().pendingKill).toBeNull();
    expect(api.releaseWorktreeCleanupBlocker).not.toHaveBeenCalled();
  });

  it('offers no kill button before a process was diagnosed', () => {
    openWith([ROW]);
    render(<BlockedCleanupDialog />);

    expect(screen.queryByRole('button', { name: /end process/i })).toBeNull();
  });

  it('reports "already gone" rather than "removed" when the retry finds nothing', async () => {
    openWith([ROW]);
    vi.mocked(api.retryWorktreeCleanup).mockReset().mockResolvedValue({ status: 'gone', record: null });
    render(<BlockedCleanupDialog />);

    fireEvent.click(screen.getByRole('button', { name: /^Retry$/ }));

    await vi.waitFor(() =>
      expect(api.retryWorktreeCleanup).toHaveBeenCalledWith(ROW.worktree_path),
    );
    const toasts = useToastStore.getState().toasts;
    expect(
      toasts.some((toast) => /already cleaned/i.test(toast.message)),
      'a gone entry is not reported as removed',
    ).toBe(true);
    expect(toasts.some((toast) => /Worktree removed/i.test(toast.message))).toBe(false);
  });

  it('reports a live spawn taking the worktree over instead of removing it', async () => {
    openWith([ROW]);
    vi.mocked(api.retryWorktreeCleanup)
      .mockReset()
      .mockResolvedValue({ status: 'claimed', record: null });
    render(<BlockedCleanupDialog />);

    fireEvent.click(screen.getByRole('button', { name: /^Retry$/ }));

    await vi.waitFor(() =>
      expect(
        useToastStore.getState().toasts.some((toast) => /left alone.*live agent/i.test(toast.message)),
      ).toBe(true),
    );
  });

  it('says nothing was retried when the queue could not be read', async () => {
    openWith([ROW]);
    vi.mocked(api.retryWorktreeCleanup)
      .mockReset()
      .mockResolvedValue({ status: 'queue-read-failed', record: null });
    render(<BlockedCleanupDialog />);

    fireEvent.click(screen.getByRole('button', { name: /^Retry$/ }));

    await vi.waitFor(() =>
      expect(
        useToastStore.getState().toasts.some((toast) => /nothing was retried/i.test(toast.message)),
      ).toBe(true),
    );
    expect(useBlockedCleanupStore.getState().entries).toHaveLength(1);
  });

  it('closes on the Close button without touching the queue', () => {
    openWith([ROW]);
    render(<BlockedCleanupDialog />);

    fireEvent.click(screen.getByRole('button', { name: /^Close$/ }));

    expect(useBlockedCleanupStore.getState().isOpen).toBe(false);
    expect(api.dismissWorktreeCleanup).not.toHaveBeenCalled();
  });

  // ── issue #2139 review round 1: the clock ──────────────────────────────────
  //
  // App.tsx used to render this dialog unconditionally, so it mounted at boot
  // and the labels were measured from boot rather than from the failure. The
  // reproduction: mount at 09:00, make the failure happen at 12:00 with the last
  // attempt 10 minutes earlier and the next retry due in 2 minutes. The old
  // code rendered "just now" and "backing off for another 3h".
  describe('time labels are measured from now, not from mount', () => {
    const MOUNT_AT = Date.UTC(2026, 9, 9, 9, 0, 0);
    const FAILURE_AT = Date.UTC(2026, 9, 9, 12, 0, 0);

    beforeEach(() => {
      vi.useFakeTimers();
      vi.setSystemTime(MOUNT_AT);
    });

    afterEach(() => {
      vi.useRealTimers();
    });

    it('reads the clock when the dialog opens, not when it mounts', async () => {
      // Mount three hours before the failure happens.
      render(<BlockedCleanupDialog />);
      expect(screen.queryByText(/Worktree cleanup blocked/)).toBeNull();

      // The failure happens at 12:00; the last attempt was 10 minutes ago and
      // the next retry is due in 2 minutes.
      act(() => {
        openWith([
          blockedRow({
            last_attempt_at: FAILURE_AT - 10 * 60 * 1000,
            retry_not_before: FAILURE_AT + 2 * 60 * 1000,
          }),
        ]);
        vi.setSystemTime(FAILURE_AT);
      });

      expect(screen.getByText(/10m ago/)).toBeTruthy();
      expect(screen.getByText(/backing off for another 2m/)).toBeTruthy();
      expect(screen.queryByText(/just now/)).toBeNull();
      expect(screen.queryByText(/3h/)).toBeNull();
    });

    it('keeps the countdown live while it is open', async () => {
      render(<BlockedCleanupDialog />);
      act(() => {
        openWith([
          blockedRow({
            last_attempt_at: FAILURE_AT - 10 * 60 * 1000,
            retry_not_before: FAILURE_AT + 2 * 60 * 1000,
          }),
        ]);
        vi.setSystemTime(FAILURE_AT);
      });
      expect(screen.getByText(/backing off for another 2m/)).toBeTruthy();

      act(() => {
        vi.advanceTimersByTime(60_000);
        vi.setSystemTime(FAILURE_AT + 60_000);
      });

      expect(screen.getByText(/backing off for another 1m/)).toBeTruthy();
    });
  });
});
