import { beforeAll, beforeEach, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { preloadProbeTabs } from './helpers/preloadProbeTabs';
import { ProbePanel } from '../../src/components/Probe/ProbePanel';
import { NotificationButton } from '../../src/components/TitleBar/NotificationButton';
import { reportBlockedWorktreeCleanup } from '../../src/lib/worktreeCleanupAlerts';
import { reportCircuitInput, watchAgentInputNotifications } from '../../src/lib/agentInputNotifications';
import { needsAttention } from '../../src/lib/notifications';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import { makeAgentNode } from './helpers/makeAgentNode';
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

it('lets a missing-agent request be dismissed in-session without treating store absence as resolution', async () => {
  vi.mocked(invoke).mockResolvedValue([]);
  seedAgentNodes([]);
  const stop = watchAgentInputNotifications();
  try {
    reportCircuitInput({ node_id: 6377, issue: 0 });
    seedAgentNodes([makeAgentNode({ id: 1, mesh_id: 1, position: 0 })]);
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
    render(<><NotificationButton /><ProbePanel /></>);
    const bell = screen.getByRole('button', { name: 'Open Notifications' });
    fireEvent.click(bell);
    const attention = await screen.findByRole('region', { name: 'Needs attention' });
    fireEvent.click(within(attention).getByRole('button', { name: /Open agent/ }));
    expect(within(attention).getByRole('alert').textContent).toContain('no longer available');
    fireEvent.click(within(attention).getByRole('button', { name: /^Dismiss notification from Circuit agent:/ }));
    expect(useToastStore.getState().notifications.filter(needsAttention)).toEqual([]);
    expect(useToastStore.getState().toasts).toEqual([]);
    expect(bell.title).toContain('0 need attention');
    expect(bell.title).toContain('0 unread');
    expect(within(screen.getByRole('region', { name: 'Recent activity' })).getByText('Notification dismissed.')).toBeTruthy();
  } finally { stop(); }
});

it('names each dismiss control with its agent and Mesh and clears only the selected request', async () => {
  vi.mocked(invoke).mockResolvedValue([]);
  addToast('Needs input', 'reviewer in buildmesh needs your input.', 'warning', {
    kind: 'agent-input', nodeId: 10, agentName: 'reviewer', meshName: 'buildmesh',
  });
  addToast('Needs input', 'reviewer in playground needs your input.', 'warning', {
    kind: 'agent-input', nodeId: 20, agentName: 'reviewer', meshName: 'playground',
  });
  render(<><NotificationButton /><ProbePanel /></>);
  const bell = screen.getByRole('button', { name: 'Open Notifications' });
  fireEvent.click(bell);
  const attention = await screen.findByRole('region', { name: 'Needs attention' });
  const dismissBuildmesh = within(attention).getByRole('button', { name: /^Dismiss notification from reviewer in buildmesh:/ });
  expect(within(attention).getByRole('button', { name: /^Dismiss notification from reviewer in playground:/ })).toBeTruthy();
  fireEvent.click(dismissBuildmesh);
  expect(within(attention).queryByRole('button', { name: /^Dismiss notification from reviewer in buildmesh:/ })).toBeNull();
  expect(within(attention).getByRole('button', { name: /^Dismiss notification from reviewer in playground:/ })).toBeTruthy();
  expect(useToastStore.getState().notifications.filter(needsAttention).map(n => n.action?.kind === 'agent-input' && n.action.nodeId)).toEqual([20]);
  expect(bell.title).toContain('1 need attention');
  expect(bell.title).toContain('1 unread');
});

it.each([
  { scenario: 'different questions', messages: ['Use main or develop?', 'May I run the checks?'] },
  { scenario: 'identical questions and arrival times', messages: ['May I run the checks?', 'May I run the checks?'] },
])('distinguishes same-named agents in one Mesh with $scenario', async ({ messages }) => {
  vi.mocked(invoke).mockResolvedValue([]);
  const clock = vi.spyOn(Date, 'now').mockReturnValue(1793523600000);
  try {
    for (const [index, message] of messages.entries()) {
      addToast('Needs input', message, 'warning', {
        kind: 'agent-input', nodeId: index === 0 ? 10 : 20, agentName: 'reviewer', meshName: 'buildmesh',
      });
    }
  } finally { clock.mockRestore(); }
  expect(new Set(useToastStore.getState().notifications.map(n => n.createdAt)).size).toBe(1);
  render(<><NotificationButton /><ProbePanel /></>);
  fireEvent.click(screen.getByRole('button', { name: 'Open Notifications' }));
  const attention = await screen.findByRole('region', { name: 'Needs attention' });
  const controls = within(attention).getAllByRole('button', { name: /^Dismiss notification from reviewer in buildmesh/ });
  const labels = controls.map(button => button.getAttribute('aria-label'));
  expect(new Set(labels).size).toBe(2);
  expect(labels[0]).toContain(messages[1]);
  expect(labels[1]).toContain(messages[0]);
  expect(labels[0]).toContain('(request 1 of 2)');
  expect(labels[1]).toContain('(request 2 of 2)');
  fireEvent.click(controls[1]);
  expect(useToastStore.getState().notifications.filter(needsAttention).map(n => n.action?.kind === 'agent-input' && n.action.nodeId)).toEqual([20]);
  const remaining = within(attention).getByRole('button', { name: /^Dismiss notification from reviewer in buildmesh/ });
  expect(remaining.getAttribute('aria-label')).toContain(messages[1]);
  expect(remaining.getAttribute('aria-label')).toContain('(request 1 of 1)');
});

// A mesh that never reached upstream and a worktree that cannot be removed are
// unrelated problems with different recoveries, so they get different cards —
// and only the sync card retries anything.
describe('sync failures in Needs attention', () => {
  const SYNC_ACTION = {
    kind: 'mesh-sync-failure' as const, path: '/repos/mesh-a', meshName: 'mesh-a',
    detail: 'Fetched 1 new commit but fast-forward failed: not possible to fast-forward',
    evidence: 'behind',
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

  it('claims nothing about the position when the fetch did not complete', async () => {
    // A failed fetch is NOT a fetch that never ran: Rust launches `git fetch`
    // and reports the failure afterwards, so upstream may have been contacted.
    // The card must not assert lag, and must not assert the opposite either —
    // it supports no claim at all. This is the same wording the `unverified`
    // and legacy-record cases render, which is the point: one honest reading
    // for every "we don't know" outcome.
    addToast('Git sync', 'mesh-a: Fetch failed: could not resolve host', 'error',
      { ...SYNC_ACTION, detail: 'Fetch failed: could not resolve host', evidence: 'unknown' });
    render(<><NotificationButton /><ProbePanel /></>);
    fireEvent.click(screen.getByRole('button', { name: 'Open Notifications' }));

    const card = await screen.findByTestId('sync-attention-/repos/mesh-a');
    expect(within(card).getByText('Upstream state unknown — this sync could not be verified')).toBeTruthy();
    expect(within(card).queryByText(/Behind upstream/)).toBeNull();
    expect(within(card).queryByText(/fetch did not run/)).toBeNull();
  });

  it('claims nothing when a fetch happened but the comparison failed', async () => {
    // Fetched, so the refs moved — but nobody established where this mesh
    // stands. Neither "behind" nor "the fetch did not run" is supported.
    addToast('Git sync', 'mesh-a: could not compare with upstream', 'error',
      { ...SYNC_ACTION, detail: 'Fetched, but could not compare with upstream: lock contention', evidence: 'unknown' });
    render(<><NotificationButton /><ProbePanel /></>);
    fireEvent.click(screen.getByRole('button', { name: 'Open Notifications' }));

    const card = await screen.findByTestId('sync-attention-/repos/mesh-a');
    expect(within(card).getByText('Upstream state unknown — this sync could not be verified')).toBeTruthy();
    expect(within(card).queryByText(/Behind upstream/)).toBeNull();
    expect(within(card).queryByText(/fetch did not run/)).toBeNull();
  });

  it('claims nothing for a record written before the evidence field existed', async () => {
    // A pre-`evidence` record may have been a fetched partial; it may equally
    // have been a fetch that never ran. The stored record supports neither, so
    // the card has to say so rather than pick the more alarming reading.
    const legacy = { kind: 'mesh-sync-failure' as const, path: '/repos/mesh-a', meshName: 'mesh-a', detail: 'Fetch failed: network down' };
    addToast('Git sync', 'mesh-a: Fetch failed: network down', 'error', legacy as typeof SYNC_ACTION);
    render(<><NotificationButton /><ProbePanel /></>);
    fireEvent.click(screen.getByRole('button', { name: 'Open Notifications' }));

    const card = await screen.findByTestId('sync-attention-/repos/mesh-a');
    expect(within(card).getByText('Upstream state unknown — this sync could not be verified')).toBeTruthy();
    expect(within(card).queryByText(/Behind upstream/)).toBeNull();
    expect(within(card).queryByText(/fetch did not run/)).toBeNull();
  });

  it('disables Retry while a sync is in flight', async () => {
    // The control must read as unavailable, not merely refuse clicks: the
    // style rule keys off `disabled` (probe-ui-checklist §4), and a
    // long-running action that stays visually prominent invites a double-click
    // that fires two syncs.
    let release: (v: unknown) => void = () => {};
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'list_pending_worktree_removals') return Promise.resolve([entry]);
      if (cmd === 'git_sync') return new Promise(resolve => { release = resolve; });
      return Promise.resolve([]);
    });
    addToast('Git sync', 'mesh-a: Fetch failed', 'error', SYNC_ACTION);
    render(<><NotificationButton /><ProbePanel /></>);
    fireEvent.click(screen.getByRole('button', { name: 'Open Notifications' }));

    const retry = await screen.findByRole('button', { name: 'Retry sync for mesh-a' });
    expect((retry as HTMLButtonElement).disabled).toBe(false);

    fireEvent.click(retry);
    await waitFor(() => expect((screen.getByRole('button', { name: 'Retry sync for mesh-a' }) as HTMLButtonElement).disabled).toBe(true));
    expect(screen.getByText('Retrying…')).toBeTruthy();

    release({ fetched: true, pulled: true, new_commits: 2, message: 'Pulled 2 new commits', status: 'synced' });
    await waitFor(() => expect(screen.queryByTestId('sync-attention-/repos/mesh-a')).toBeNull());
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
