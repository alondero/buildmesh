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

// A mesh that never reached upstream and a worktree that cannot be removed are
// unrelated problems with different recoveries, so they get different cards —
// and only the sync card retries anything.
describe('sync failures in Needs attention', () => {
  const SYNC_ACTION = {
    kind: 'mesh-sync-failure' as const, path: '/repos/mesh-a', meshName: 'mesh-a',
    detail: 'Fetched 1 new commit but fast-forward failed: not possible to fast-forward',
    refsFetched: true,
  };

  it('names the mesh and its reason, and counts it as needing attention', async () => {
    addToast('Git sync', 'mesh-a: Fetch failed', 'error', SYNC_ACTION);
    render(<><NotificationButton /><ProbePanel /></>);
    expect(screen.getByRole('button', { name: 'Open Notifications' }).title).toContain('1 need attention');
    fireEvent.click(screen.getByRole('button', { name: 'Open Notifications' }));
    const card = await screen.findByTestId('sync-attention-/repos/mesh-a');
    expect(within(card).getByText('mesh-a')).toBeTruthy();
    expect(within(card).getByText(/fast-forward failed/)).toBeTruthy();
    expect(within(card).getByText('/repos/mesh-a')).toBeTruthy();
    // Fetched, so upstream state is known and the mesh is genuinely behind.
    expect(within(card).getByText('Behind upstream — the fetched commits were not applied')).toBeTruthy();
  });

  it('does not claim the mesh is behind when the fetch never ran', async () => {
    // A network outage on an already-current mesh must not be reported as
    // confirmed lag: the backend never contacted upstream, so it established
    // nothing about this mesh's position.
    addToast('Git sync', 'mesh-a: Fetch failed: could not resolve host', 'error',
      { ...SYNC_ACTION, detail: 'Fetch failed: could not resolve host', refsFetched: false });
    render(<><NotificationButton /><ProbePanel /></>);
    fireEvent.click(screen.getByRole('button', { name: 'Open Notifications' }));

    const card = await screen.findByTestId('sync-attention-/repos/mesh-a');
    expect(within(card).getByText('Upstream not checked — the fetch did not run')).toBeTruthy();
    expect(within(card).queryByText(/Behind upstream/)).toBeNull();
  });

  it('retries the sync from the card and clears it once the mesh syncs', async () => {
    vi.mocked(invoke).mockImplementation(async cmd => {
      if (cmd === 'list_pending_worktree_removals') return [entry];
      if (cmd === 'git_sync') {
        return { fetched: true, pulled: true, new_commits: 2, message: 'Pulled 2 new commits', status: 'synced' };
      }
      return [];
    });
    addToast('Git sync', 'mesh-a: Fetch failed', 'error', SYNC_ACTION);
    render(<><NotificationButton /><ProbePanel /></>);
    fireEvent.click(screen.getByRole('button', { name: 'Open Notifications' }));

    fireEvent.click(await screen.findByRole('button', { name: 'Retry sync for mesh-a' }));

    await waitFor(() => expect(vi.mocked(invoke)).toHaveBeenCalledWith('git_sync', { path: '/repos/mesh-a' }));
    // A successful retry is the resolution of the card it was clicked from —
    // otherwise a fixed mesh stays in Needs attention forever.
    await waitFor(() => expect(screen.queryByTestId('sync-attention-/repos/mesh-a')).toBeNull());
    expect(screen.getByText(/Synced successfully\./)).toBeTruthy();
  });

  it('keeps the card and raises a new failure when the retry fails again', async () => {
    vi.mocked(invoke).mockImplementation(async cmd => {
      if (cmd === 'list_pending_worktree_removals') return [entry];
      if (cmd === 'git_sync') {
        return { fetched: false, pulled: false, new_commits: 0, message: 'Fetch failed: still down', status: 'failed' };
      }
      return [];
    });
    addToast('Git sync', 'mesh-a: Fetch failed', 'error', SYNC_ACTION);
    render(<><NotificationButton /><ProbePanel /></>);
    fireEvent.click(screen.getByRole('button', { name: 'Open Notifications' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Retry sync for mesh-a' }));

    // The 30-second worktree reconciliation also runs on mount; it must not
    // resolve the sync card, or the failure would vanish on its own.
    await waitFor(() => expect(screen.getByText(/still down/)).toBeTruthy());
    expect(screen.getByTestId('sync-attention-/repos/mesh-a')).toBeTruthy();
  });

  it('offers a worktree recovery only for the worktree problem', async () => {
    addToast('Git sync', 'mesh-a: Fetch failed', 'error', SYNC_ACTION);
    await act(async () => reportBlockedWorktreeCleanup({
      node_name: entry.node_name, worktree_path: entry.worktree_path, operation: entry.last_operation!,
      error: entry.last_error!, attempt_count: 1, last_attempt_at: entry.last_attempt_at, retry_not_before: entry.retry_not_before,
    }));
    render(<><NotificationButton /><ProbePanel /></>);
    fireEvent.click(screen.getByRole('button', { name: 'Open Notifications' }));

    expect(await screen.findByRole('button', { name: 'Resolve worktree blocked' })).toBeTruthy();
    expect(screen.getByRole('button', { name: 'Retry sync for mesh-a' })).toBeTruthy();
    // Resolving the worktree must leave the sync card alone.
    fireEvent.click(screen.getByRole('button', { name: 'Resolve worktree blocked' }));
    expect(await screen.findByRole('dialog')).toBeTruthy();
    expect(screen.getByTestId('sync-attention-/repos/mesh-a')).toBeTruthy();
  });
});
