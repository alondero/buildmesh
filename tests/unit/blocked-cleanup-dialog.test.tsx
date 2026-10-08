/**
 * Blocked worktree cleanup dialog (issue #2139).
 *
 * The dialog is the recovery surface for a worktree removal that could not
 * complete. What matters is that it shows the *evidence* the backend persisted
 * (path, failed step, error, attempts, backoff) rather than a generic warning,
 * and that the four recoveries — copy path, copy diagnostics, retry, keep
 * worktree — plus the explicit process diagnosis and targeted kill are all
 * reachable.
 */

import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import { BlockedCleanupDialog } from '../../src/components/BlockedCleanupDialog/BlockedCleanupDialog';
import { useBlockedCleanupStore } from '../../src/stores/blockedCleanupStore';
import * as api from '../../src/lib/tauri';
import type { PendingWorktreeRemoval } from '../../src/types/generated/PendingWorktreeRemoval';

vi.mock('../../src/lib/tauri', () => ({
  listPendingWorktreeRemovals: vi.fn(),
  retryWorktreeCleanup: vi.fn(),
  dismissWorktreeCleanup: vi.fn(),
  diagnoseWorktreeCleanupBlockers: vi.fn(),
  releaseWorktreeCleanupBlocker: vi.fn(),
}));

const BLOCKED: PendingWorktreeRemoval = {
  worktree_path: 'C:/repo/.claude/worktrees/agent-node',
  node_name: 'agent-node',
  attempt_count: 3,
  last_attempt_at: Date.now(),
  last_operation: 'rename-worktree-to-staging',
  last_error: 'The process cannot access the file because it is being used by another process. (os error 32)',
  retry_not_before: Date.now() + 60_000,
  notified_error: 'rename-worktree-to-staging: os error 32',
};

function openWith(entries: PendingWorktreeRemoval[]) {
  useBlockedCleanupStore.setState({
    entries,
    isOpen: true,
    loading: false,
    blockers: {},
    actionStatus: {},
  });
}

describe('BlockedCleanupDialog (#2139)', () => {
  beforeEach(() => {
    useBlockedCleanupStore.setState({ entries: [], isOpen: false, loading: false, blockers: {}, actionStatus: {} });
    const writeText = vi.fn(() => Promise.resolve());
    Object.defineProperty(navigator, 'clipboard', { value: { writeText }, configurable: true });
    vi.mocked(api.retryWorktreeCleanup).mockReset().mockResolvedValue(BLOCKED);
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
    openWith([BLOCKED]);
    render(<BlockedCleanupDialog />);

    expect(screen.getByText('agent-node')).toBeTruthy();
    expect(screen.getByText(BLOCKED.worktree_path)).toBeTruthy();
    expect(screen.getByText(/Moving the folder aside to remove it/)).toBeTruthy();
    expect(screen.getByText(/os error 32/)).toBeTruthy();
    expect(screen.getByText('3')).toBeTruthy(); // attempt count is shown
  });

  it('offers copy path, copy diagnostics, diagnose, retry and keep worktree', () => {
    openWith([BLOCKED]);
    render(<BlockedCleanupDialog />);

    expect(screen.getByRole('button', { name: 'Copy path' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Copy diagnostics' })).toBeTruthy();
    expect(screen.getByRole('button', { name: /what is holding it/i })).toBeTruthy();
    expect(screen.getByRole('button', { name: /^Retry$/ })).toBeTruthy();
    expect(screen.getByRole('button', { name: /keep worktree/i })).toBeTruthy();
  });

  it('copies the bare path, not the diagnostics block', async () => {
    openWith([BLOCKED]);
    render(<BlockedCleanupDialog />);

    fireEvent.click(screen.getByRole('button', { name: 'Copy path' }));

    expect(navigator.clipboard.writeText).toHaveBeenCalledTimes(1);
    expect((navigator.clipboard.writeText as unknown as { mock: { calls: string[][] } }).mock.calls[0][0]).toBe(
      BLOCKED.worktree_path,
    );
  });

  it('copies the diagnostics block with the blocker evidence attached', async () => {
    openWith([BLOCKED]);
    useBlockedCleanupStore.setState({
      blockers: {
        [BLOCKED.worktree_path]: [
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
    expect(call).toContain(BLOCKED.worktree_path);
    expect(call).toContain('os error 32');
    expect(call).toContain('powershell.exe (pid 5212)');
  });

  it('retries the cleanup for that path when Retry is pressed', async () => {
    openWith([BLOCKED]);
    render(<BlockedCleanupDialog />);

    fireEvent.click(screen.getByRole('button', { name: /^Retry$/ }));

    await vi.waitFor(() => expect(api.retryWorktreeCleanup).toHaveBeenCalledWith(BLOCKED.worktree_path));
  });

  it('diagnoses processes on request and offers a targeted kill per row', async () => {
    openWith([BLOCKED]);
    vi.mocked(api.diagnoseWorktreeCleanupBlockers).mockResolvedValue([
      {
        pid: 5212,
        name: 'powershell.exe',
        executable_path: 'C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe',
        reason: 'working-directory',
        detail: 'C:/repo/.claude/worktrees/agent-node/child',
      },
    ]);
    render(<BlockedCleanupDialog />);

    fireEvent.click(screen.getByRole('button', { name: /what is holding it/i }));
    await vi.waitFor(() => expect(screen.getByText(/powershell\.exe \(pid 5212\)/)).toBeTruthy());

    const kill = screen.getByRole('button', { name: /end process/i });
    fireEvent.click(kill);
    await vi.waitFor(() => expect(api.releaseWorktreeCleanupBlocker).toHaveBeenCalledWith(5212));
  });

  it('offers no kill button before a process was diagnosed', () => {
    openWith([BLOCKED]);
    render(<BlockedCleanupDialog />);

    expect(screen.queryByRole('button', { name: /end process/i })).toBeNull();
  });

  it('closes on the Close button without touching the queue', () => {
    openWith([BLOCKED]);
    render(<BlockedCleanupDialog />);

    fireEvent.click(screen.getByRole('button', { name: /^Close$/ }));

    expect(useBlockedCleanupStore.getState().isOpen).toBe(false);
    expect(api.dismissWorktreeCleanup).not.toHaveBeenCalled();
  });
});
