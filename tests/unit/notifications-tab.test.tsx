import { beforeAll, beforeEach, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { preloadProbeTabs } from './helpers/preloadProbeTabs';
import { ProbePanel } from '../../src/components/Probe/ProbePanel';
import { NotificationButton } from '../../src/components/TitleBar/NotificationButton';
import { reportBlockedWorktreeCleanup } from '../../src/lib/worktreeCleanupAlerts';
import { addToast, useToastStore } from '../../src/stores/toastStore';
import { useBlockedCleanupStore } from '../../src/stores/blockedCleanupStore';
import { useUIStore } from '../../src/stores/uiStore';
import { useMeshStore } from '../../src/stores/meshStore';
import type { PendingWorktreeRemoval } from '../../src/types/generated/PendingWorktreeRemoval';

beforeAll(preloadProbeTabs, 60000);
const entry: PendingWorktreeRemoval = {
  node_name: 'blocked', worktree_path: '/repo/worktrees/blocked', attempt_count: 1,
  last_attempt_at: 1700000000000, last_error: 'Folder in use', last_operation: 'rename-worktree-to-staging',
  retry_not_before: 1700000005000, notified_at: 1700000000000, notified_error: 'Folder in use',
};
beforeEach(() => {
  useUIStore.setState({ probeOpen: false, probeTab: 'files', probeWorkingSet: { tabs: [], mru: [] } });
  useMeshStore.setState({ selectedMeshId: null, meshesById: new Map() });
  useToastStore.setState({ toasts: [], notifications: [], cleanupError: null, cleanupLoading: false, worktreeRevision: 0 });
  useBlockedCleanupStore.setState({ isOpen: false, entries: [], loading: false, blockers: {}, actionStatus: {}, pendingKill: null });
  vi.mocked(invoke).mockImplementation(async cmd => {
    if (cmd === 'list_pending_worktree_removals') return [entry];
    if (cmd === 'retry_worktree_cleanup') return { status: 'removed', record: null };
    return [];
  });
});

it('opens the Host Probe from the bell and only shows recovery after explicit Resolve', async () => {
  render(<><NotificationButton /><ProbePanel /></>);
  await act(async () => reportBlockedWorktreeCleanup({
    node_name: entry.node_name, worktree_path: entry.worktree_path, operation: entry.last_operation!,
    error: entry.last_error!, attempt_count: 1, last_attempt_at: entry.last_attempt_at, retry_not_before: entry.retry_not_before,
  }));
  expect(screen.queryByRole('dialog')).toBeNull();
  const bell = screen.getByRole('button', { name: 'Open Notifications' });
  expect(bell.title).toContain('1 need attention');
  fireEvent.click(bell);
  expect(await screen.findByTestId('notifications-tab')).toBeTruthy();
  expect(screen.getByText('Host-wide')).toBeTruthy();
  await waitFor(() => expect(useToastStore.getState().cleanupLoading).toBe(false));
  fireEvent.click(screen.getByRole('button', { name: 'Mark all read' }));
  expect(screen.getByRole('button', { name: 'Resolve worktree blocked' })).toBeTruthy();
  expect(bell.title).toContain('1 need attention');
  expect(screen.queryByRole('dialog')).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: 'Resolve worktree blocked' }));
  const dialog = await screen.findByRole('dialog');
  fireEvent.click(within(dialog).getByRole('button', { name: 'Retry' }));
  await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  expect(screen.queryByRole('button', { name: 'Resolve worktree blocked' })).toBeNull();
  expect(screen.getByText('Removed.')).toBeTruthy();
});

it('keeps read history after toast dismissal, clears it explicitly, and swaps with another Probe', async () => {
  vi.mocked(invoke).mockResolvedValue([]);
  addToast('Sync', 'Latest activity', 'info');
  useToastStore.getState().dismissToast(useToastStore.getState().toasts[0].id);
  render(<><NotificationButton /><ProbePanel /></>);
  fireEvent.click(screen.getByRole('button', { name: 'Open Notifications' }));
  expect(await screen.findByText('Latest activity')).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: 'Mark all read' }));
  fireEvent.click(screen.getByRole('button', { name: 'Clear read' }));
  expect(screen.getByText('No notifications yet')).toBeTruthy();
  await act(async () => useUIStore.getState().openProbeTab('usage'));
  expect(screen.queryByTestId('notifications-tab')).toBeNull();
  fireEvent.click(screen.getByRole('button', { name: 'Open Notifications' }));
  expect(await screen.findByTestId('notifications-tab')).toBeTruthy();
  fireEvent.click(screen.getByRole('button', { name: 'Open Notifications' }));
  expect(screen.queryByTestId('notifications-tab')).toBeNull();
});
