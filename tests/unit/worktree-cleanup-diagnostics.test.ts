/**
 * Blocked worktree cleanup diagnostics (issue #2139).
 *
 * The old toast hid the worktree path and the error reason, gave no recovery
 * action, and repeated itself on every drain. These tests pin the two halves
 * the new surface is judged on: the diagnostics block a user can copy, and the
 * labels that make a persisted queue row readable at a glance.
 */

import { describe, it, expect, vi } from 'vitest';
import {
  formatCleanupDiagnostics,
  lastAttemptLabel,
  nextRetryLabel,
  operationLabel,
} from '../../src/lib/worktreeCleanupDiagnostics';
import type { PendingWorktreeRemoval } from '../../src/types/generated/PendingWorktreeRemoval';
import type { BlockingProcess } from '../../src/types/generated/BlockingProcess';

const BLOCKED: PendingWorktreeRemoval = {
  worktree_path: 'C:/repo/.claude/worktrees/agent-node',
  node_name: 'agent-node',
  attempt_count: 3,
  last_attempt_at: 1_700_000_000_000,
  last_operation: 'rename-worktree-to-staging',
  last_error: 'The process cannot access the file because it is being used by another process. (os error 32)',
  retry_not_before: 1_700_000_300_000,
  notified_error: 'rename-worktree-to-staging: The process cannot access the file because it is being used by another process. (os error 32)',
};

describe('worktree cleanup diagnostics (#2139)', () => {
  it('names the failed operation in words a user can act on', () => {
    expect(operationLabel('rename-worktree-to-staging')).toMatch(/folder aside/i);
    expect(operationLabel('delete-staged-worktree')).toMatch(/deleting/i);
    expect(operationLabel('never-heard-of-this-step')).toBe(
      'never-heard-of-this-step',
      'an unknown operation still renders, so a new backend step cannot blank the UI',
    );
    expect(operationLabel(null)).toMatch(/no removal attempt/i);
  });

  it('shows when the last attempt ran', () => {
    expect(lastAttemptLabel(BLOCKED, 1_700_000_000_000)).toMatch(/just now/);
    expect(lastAttemptLabel(BLOCKED, 1_700_000_060_000)).toMatch(/1m ago/);
    const fresh = { ...BLOCKED, last_attempt_at: 0 };
    expect(lastAttemptLabel(fresh, 1_700_000_060_000)).toBe('never attempted');
  });

  it('reads the backoff as a countdown, "due now" and the launch fallback', () => {
    const now = 1_700_000_000_000;
    expect(nextRetryLabel(BLOCKED, now)).toBe('backing off for another 5m');
    expect(nextRetryLabel(BLOCKED, 1_700_000_299_000)).toBe('backing off for another 1s');
    expect(nextRetryLabel(BLOCKED, 1_700_000_400_000)).toBe('due now');
    expect(nextRetryLabel({ ...BLOCKED, retry_not_before: 0 }, now)).toBe(
      'on the next launch or close',
    );
    expect(nextRetryLabel({ ...BLOCKED, last_attempt_at: 0 }, now)).toBe(
      'on the next launch or close',
    );
  });

  it('copies everything a maintainer needs to diagnose the block', () => {
    const blockers: BlockingProcess[] = [
      {
        pid: 5212,
        name: 'powershell.exe',
        executable_path: 'C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe',
        reason: 'working-directory',
        detail: 'C:/repo/.claude/worktrees/agent-node/child',
      },
    ];

    const text = formatCleanupDiagnostics(BLOCKED, blockers);

    expect(text).toContain('Node: agent-node');
    expect(text).toContain('Worktree path: C:/repo/.claude/worktrees/agent-node');
    expect(text).toContain('Failed operation: rename-worktree-to-staging');
    expect(text).toContain('Last error: The process cannot access the file');
    expect(text).toContain('os error 32');
    expect(text).toContain('Attempts: 3');
    // The diagnosis is part of the diagnostics, not a separate surface.
    expect(text).toContain('Processes holding the folder (1)');
    expect(text).toContain('powershell.exe (pid 5212) — working-directory');
    expect(text).toContain('C:/repo/.claude/worktrees/agent-node/child');
    // Eight fixed lines, the blocker header and one line per process.
    expect(text.split('\n').length).toBe(10);
  });

  it('says when the blocker diagnosis has not run instead of implying none exists', () => {
    // An empty diagnosis must not read as "nothing holds the folder" — the
    // error already says otherwise.
    const text = formatCleanupDiagnostics(BLOCKED, []);
    expect(text).toMatch(/not diagnosed yet/i);
    expect(text).not.toMatch(/\(0\)/);
  });

  it('clips the clipboard write to a single call with the formatted block', async () => {
    const writeText = vi.fn(() => Promise.resolve());
    // The jsdom-ish test harness has no real clipboard; install the stub the
    // dialog/App code calls.
    (navigator as unknown as { clipboard: { writeText: typeof writeText } }).clipboard = {
      writeText,
    };

    await navigator.clipboard.writeText(formatCleanupDiagnostics(BLOCKED));

    expect(writeText).toHaveBeenCalledTimes(1);
    expect(writeText.mock.calls[0][0]).toContain(
      'Worktree path: C:/repo/.claude/worktrees/agent-node',
    );
  });
});
