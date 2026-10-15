import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import {
  classifySyncResult, outcomeOf, raiseSyncAttention, reportMeshSync, reportSyncError, runMeshSync, SYNC_PROVIDER,
  useMeshSyncCompletion,
} from '../../src/lib/meshSyncFeedback';
import { addToast, useToastStore } from '../../src/stores/toastStore';
import { needsAttention } from '../../src/lib/notifications';
import type { GitSyncResult } from '../../src/types/generated/GitSyncResult';
import type { GitSyncStatus } from '../../src/types/generated/GitSyncStatus';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

const MESH = { name: 'lambo-recomp', path: '/repos/lambo-recomp' };

/** Mirrors the backend's own mapping (see `sync_outcome_to_git_sync_result`):
 *  a `skipped` or `failed` run never reached the remote, so it reports
 *  `fetched: false`. A helper that defaulted `fetched: true` would make the
 *  "no completion was published" and "reads the flag, not the status" cases
 *  assert against a shape the backend cannot produce. */
const FETCHED_BY_STATUS: Record<GitSyncStatus, boolean> = {
  synced: true, up_to_date: true, skipped: false, partial: true, failed: false,
};

function result(status: GitSyncStatus, message: string, overrides: Partial<GitSyncResult> = {}): GitSyncResult {
  return { fetched: FETCHED_BY_STATUS[status], pulled: false, new_commits: 0, message, status, ...overrides };
}

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  useToastStore.setState({ toasts: [], notifications: [], worktreeRevision: 0, cleanupError: null, cleanupLoading: false });
  useMeshSyncCompletion.setState({ completions: {} });
});

describe('sync result classification', () => {
  // The severity table is the whole feature: `MeshItem` used to discard this
  // result entirely, so nothing pinned it. `partial` and `failed` are the two
  // that must reach the user as a problem rather than a neutral notice.
  it.each([
    ['synced', 'success', false, true],
    ['up_to_date', 'info', false, true],
    ['skipped', 'info', false, false],
    ['partial', 'warning', true, false],
    ['failed', 'error', true, false],
  ] as const)('maps %s to %s (unresolved: %s, verified: %s)', (status, severity, unresolved, verified) => {
    expect(classifySyncResult(result(status, 'x'))).toEqual({ severity, unresolved, verified });
  });

  it('treats a remote-less mesh as resolved, not as a problem to nag about', () => {
    // `skipped` and `failed` share `fetched: false, new_commits: 0`; only the
    // status separates a valid local-only repo from a real failure.
    expect(classifySyncResult(result('skipped', 'No remote configured')).unresolved).toBe(false);
    expect(classifySyncResult(result('failed', 'Fetch failed: network down')).unresolved).toBe(true);
  });

  it('distinguishes "checked and in sync" from "checked nothing"', () => {
    // A skipped sync contacted no remote. Treating it as verified would let it
    // resolve a real failure it never retested, and would let a bulk summary
    // count it as a success the run never achieved.
    expect(classifySyncResult(result('up_to_date', 'Already up to date')).verified).toBe(true);
    expect(classifySyncResult(result('skipped', 'No remote configured')).verified).toBe(false);
  });
});

describe('reporting a manual sync', () => {
  it('always raises a toast naming the mesh, whatever the outcome', () => {
    reportMeshSync(MESH, result('synced', 'Pulled 3 new commits', { pulled: true, new_commits: 3 }));
    const [toast] = useToastStore.getState().toasts;
    expect(toast).toMatchObject({ provider: SYNC_PROVIDER, message: 'lambo-recomp: Pulled 3 new commits', severity: 'success' });
  });

  it('puts a partial sync into Needs attention with the backend\'s own reason', () => {
    const detail = 'Fetched 1 new commit but fast-forward failed: not possible to fast-forward';
    reportMeshSync(MESH, result('partial', detail));
    const [notice] = useToastStore.getState().notifications;
    expect(needsAttention(notice)).toBe(true);
    expect(notice.action).toEqual({ kind: 'mesh-sync-failure', path: MESH.path, meshName: MESH.name, detail });
    // The reason is the whole point: a diverged history and a dirty path are
    // different recoveries, and the UI has no way to invent that text.
    expect(notice.message).toContain('fast-forward failed');
  });

  it('puts a failed fetch into Needs attention', () => {
    reportMeshSync(MESH, result('failed', 'Fetch failed: could not resolve host'));
    expect(needsAttention(useToastStore.getState().notifications[0])).toBe(true);
  });

  it('does not put a successful or up-to-date sync into Needs attention', () => {
    reportMeshSync(MESH, result('up_to_date', 'Already up to date'));
    expect(useToastStore.getState().notifications.some(needsAttention)).toBe(false);
  });

  it('resolves an earlier failure once a later sync succeeds', () => {
    reportMeshSync(MESH, result('failed', 'Fetch failed: network down'));
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);

    reportMeshSync(MESH, result('synced', 'Pulled 1 new commit', { pulled: true, new_commits: 1 }));
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(0);
    // The problem's record survives as history, marked resolved — it is not
    // deleted, so the user can still see what happened. (The successful sync's
    // own toast is a separate, action-less entry at the top of the history.)
    const resolved = useToastStore.getState().notifications.find(n => n.action?.kind === 'mesh-sync-failure');
    expect(resolved?.action).toMatchObject({ resolution: 'Synced successfully.', detail: 'Fetch failed: network down' });
  });

  it('does not resolve another mesh\'s failure', () => {
    reportMeshSync(MESH, result('failed', 'Fetch failed: network down'));
    reportMeshSync({ name: 'other', path: '/repos/other' }, result('synced', 'Pulled 1 new commit', { pulled: true, new_commits: 1 }));
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  });

  it('reports a thrown IPC as a failure carrying the thrown reason', () => {
    const feedback = reportSyncError(MESH, new Error('failed to open repo'));
    expect(feedback).toEqual({ severity: 'error', unresolved: true, verified: false });
    const [notice] = useToastStore.getState().notifications;
    expect(notice.action).toMatchObject({ kind: 'mesh-sync-failure', detail: 'failed to open repo' });
  });

  it('does not let a skipped sync resolve an earlier failure', () => {
    // The mesh was never contacted, so the earlier failure is still the current
    // truth. Clearing it here would silently drop the user's one real problem.
    reportMeshSync(MESH, result('failed', 'Fetch failed: network down'));
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);

    reportMeshSync(MESH, result('skipped', 'No remote configured'));
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  });

  it('publishes a completion only for a verified sync', () => {
    const revisions = () => useMeshSyncCompletion.getState().completions[MESH.path]?.revision ?? 0;
    expect(revisions()).toBe(0);

    reportSyncError(MESH, new Error('boom'));
    reportMeshSync(MESH, result('skipped', 'No remote configured'));
    expect(revisions()).toBe(0);

    reportMeshSync(MESH, result('synced', 'Pulled 1 new commit', { pulled: true, new_commits: 1 }));
    expect(revisions()).toBe(1);
    reportMeshSync(MESH, result('up_to_date', 'Already up to date'));
    // A counter, not a boolean: a repeat must still wake a subscriber up.
    expect(revisions()).toBe(2);
  });

  it('publishes a fetch without a verification for a partial sync', () => {
    // A partial run fetches but does not fast-forward, so the remote-tracking
    // ref the behind-count is measured against has moved and every row for this
    // mesh must re-read it — while the failure itself stays unresolved.
    const entry = () => useMeshSyncCompletion.getState().completions[MESH.path];
    reportMeshSync(MESH, result('partial', 'Fetched 1 new commit but fast-forward failed: diverged'));
    expect(entry()).toMatchObject({ verified: false, refsFetched: true });
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  });

  it('publishes no completion for a result that neither verified nor fetched', () => {
    // Publishing nothing and raising a card are separate questions, and this
    // pins the difference so the documented contract cannot drift from it: a
    // failed fetch that never reached the remote is a completed `git_sync`
    // result — the backend answered — that still moved no refs and established
    // nothing, so no row's cached state changes.
    reportMeshSync(MESH, result('failed', 'Fetch failed: could not resolve host'));
    expect(useMeshSyncCompletion.getState().completions[MESH.path]).toBeUndefined();
    // …but the user still has to be told, so it raises an unresolved card.
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
    // A mesh with no remote neither publishes nor raises: there is nothing to
    // report and nothing that is wrong.
    reportMeshSync(MESH, result('skipped', 'No remote configured'));
    expect(useMeshSyncCompletion.getState().completions[MESH.path]).toBeUndefined();
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  });

  it('reads the fetch signal from the backend flag, not the status', () => {
    // `skipped` and `failed` both carry fetched:false upstream; only a partial
    // run reports fetched:true without verifying. Deriving the signal from the
    // status instead would miss exactly that case.
    expect(outcomeOf(result('partial', 'x'))).toEqual({ verified: false, refsFetched: true });
    expect(outcomeOf(result('skipped', 'x'))).toEqual({ verified: false, refsFetched: false });
    expect(outcomeOf(result('synced', 'x', { pulled: true }))).toEqual({ verified: true, refsFetched: true });
  });
});

describe('runMeshSync', () => {
  it('invokes git_sync and reports the result it gets back', async () => {
    vi.mocked(invoke).mockResolvedValue(result('partial', 'Fetched 2 new commits; fast-forward skipped: changes to a.txt would be overwritten'));
    const { feedback, completed } = await runMeshSync(MESH);
    expect(invoke).toHaveBeenCalledWith('git_sync', { path: MESH.path });
    // `completed` is what tells the sidebar the fetch actually ran, so only
    // then is re-reading the behind count meaningful.
    expect(completed).toBe(true);
    expect(feedback.unresolved).toBe(true);
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  });

  it('reports a rejected IPC as not completed so the caller skips the refresh', async () => {
    vi.mocked(invoke).mockRejectedValue(new Error('boom'));
    const { feedback, completed } = await runMeshSync(MESH);
    expect(completed).toBe(false);
    expect(feedback).toEqual({ severity: 'error', unresolved: true, verified: false });
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  });

  it('treats an unknown status as a failure instead of crashing the sidebar', async () => {
    // The wire type is generated and drift-gated, so an unknown status can
    // only come from an app/backend version mismatch. Reporting it as a
    // failure is the safe reading; throwing would take the sidebar row down
    // and leave the user with no feedback at all.
    vi.mocked(invoke).mockResolvedValue({ fetched: true, pulled: false, new_commits: 0, message: 'who knows', status: 'invented' } as unknown as GitSyncResult);
    const { feedback, completed } = await runMeshSync(MESH);
    expect(feedback).toEqual({ severity: 'error', unresolved: true, verified: false });
    expect(completed).toBe(true);
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  });
});

describe('raiseSyncAttention', () => {
  it('reuses the existing unresolved card for the same mesh instead of stacking duplicates', () => {
    raiseSyncAttention(MESH, 'Fetch failed: network down');
    raiseSyncAttention(MESH, 'Fetch failed: still down');
    const attention = useToastStore.getState().notifications.filter(needsAttention);
    expect(attention).toHaveLength(1);
    expect(attention[0].action).toMatchObject({ detail: 'Fetch failed: still down' });
  });

  it('records the problem without a visible toast when asked to be silent', () => {
    // The bulk command shows ONE summary toast; the per-mesh problems exist in
    // Needs attention only, or a dozen of them would overflow the toast stack.
    raiseSyncAttention(MESH, 'Fetch failed: network down', 'error', { silent: true });
    expect(useToastStore.getState().toasts).toHaveLength(0);
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  });

  it('still shows a toast when a silent record has no action to file it under', () => {
    // Silently filing a record nothing can surface would be a bug, not a
    // shortcut — so the flag is ignored without an action.
    addToast(SYNC_PROVIDER, 'mesh-a: something happened', 'info', undefined, { silent: true });
    expect(useToastStore.getState().toasts).toHaveLength(1);
  });
});