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

/** One constructor per wire shape the backend actually emits, with every
 *  field written out.
 *
 *  Deliberately NOT a `status → fetched` table: that would be production
 *  mapping logic living in a test file, and it goes stale the moment an
 *  outcome is added (it already had once — `no_upstream` is fetched, while
 *  `skipped` is not, and a per-status table hides that difference). Spelling
 *  each shape out also keeps `fetched: false` honest: it means the fetch did
 *  not COMPLETE, not that it was never attempted. */
const wire = (over: Partial<GitSyncResult> & Pick<GitSyncResult, 'status' | 'fetched'>): GitSyncResult => ({
  pulled: false, new_commits: 0, message: `${over.status}`, ...over,
});

const SYNCED = (message = 'Pulled 3 new commits') => wire({ status: 'synced', fetched: true, pulled: true, new_commits: 3, message });
const UP_TO_DATE = (message = 'Already up to date') => wire({ status: 'up_to_date', fetched: true, message });
/** No remote configured — the only outcome that proves nothing was fetched. */
const NO_REMOTE = (message = 'No remote configured') => wire({ status: 'skipped', fetched: false, message });
/** A remote was fetched; the branch tracks nothing, so no comparison ran. */
const NO_UPSTREAM = (message = 'Fetched, but this branch has no upstream to compare against') => wire({ status: 'no_upstream', fetched: true, message });
const PARTIAL = (message = 'Fetched 1 new commit but fast-forward failed: diverged') => wire({ status: 'partial', fetched: true, new_commits: 1, message });
const UNVERIFIED = (message = 'Fetched, but could not compare with upstream: locked') => wire({ status: 'unverified', fetched: true, message });
/** The fetch was launched and did not complete — `fetched: false` says
 *  "did not finish", not "was never tried". */
const FAILED = (message = 'Fetch failed: could not resolve host') => wire({ status: 'failed', fetched: false, message });

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  useToastStore.setState({ toasts: [], notifications: [], worktreeRevision: 0, cleanupError: null, cleanupLoading: false });
  useMeshSyncCompletion.setState({ completions: {} });
});

describe('sync result classification', () => {
  // The severity table is the whole feature: `MeshItem` used to discard this
  // result entirely, so nothing pinned it. `partial`, `unverified` and
  // `failed` are the three that must reach the user as a problem rather than
  // a neutral notice.
  it.each([
    ['synced', SYNCED, 'success', false, true],
    ['up_to_date', UP_TO_DATE, 'info', false, true],
    ['skipped (no remote)', NO_REMOTE, 'info', false, false],
    ['no_upstream', NO_UPSTREAM, 'info', false, false],
    ['partial', PARTIAL, 'warning', true, false],
    ['unverified', UNVERIFIED, 'warning', true, false],
    ['failed', FAILED, 'error', true, false],
  ] as const)('maps %s to %s (unresolved: %s, verified: %s)', (_name, shape, severity, unresolved, verified) => {
    expect(classifySyncResult(shape())).toEqual({ severity, unresolved, verified });
  });

  it('treats a remote-less mesh as resolved, not as a problem to nag about', () => {
    // `no_remote` and `failed` share `fetched: false, new_commits: 0`; only the
    // status separates a valid local-only repo from a real failure.
    expect(classifySyncResult(NO_REMOTE()).unresolved).toBe(false);
    expect(classifySyncResult(FAILED()).unresolved).toBe(true);
  });

  it('distinguishes "checked and in sync" from "checked nothing"', () => {
    // Neither `skipped` nor `no_upstream` verified anything. Treating either as
    // verified would let it resolve a real failure it never retested, and would
    // let a bulk summary count it as a success the run never achieved.
    expect(classifySyncResult(UP_TO_DATE()).verified).toBe(true);
    expect(classifySyncResult(NO_REMOTE()).verified).toBe(false);
    expect(classifySyncResult(NO_UPSTREAM()).verified).toBe(false);
  });
});

describe('reporting a manual sync', () => {
  it('always raises a toast naming the mesh, whatever the outcome', () => {
    reportMeshSync(MESH, SYNCED());
    const [toast] = useToastStore.getState().toasts;
    expect(toast).toMatchObject({ provider: SYNC_PROVIDER, message: 'lambo-recomp: Pulled 3 new commits', severity: 'success' });
  });

  it('puts a partial sync into Needs attention with the backend\'s own reason', () => {
    const detail = 'Fetched 1 new commit but fast-forward failed: not possible to fast-forward';
    reportMeshSync(MESH, PARTIAL(detail));
    const [notice] = useToastStore.getState().notifications;
    expect(needsAttention(notice)).toBe(true);
    expect(notice.action).toEqual({ kind: 'mesh-sync-failure', path: MESH.path, meshName: MESH.name, detail, evidence: 'behind' });
    // The reason is the whole point: a diverged history and a dirty path are
    // different recoveries, and the UI has no way to invent that text.
    expect(notice.message).toContain('fast-forward failed');
  });

  it('puts a failed fetch into Needs attention', () => {
    reportMeshSync(MESH, FAILED());
    expect(needsAttention(useToastStore.getState().notifications[0])).toBe(true);
  });

  it('does not put a successful or up-to-date sync into Needs attention', () => {
    reportMeshSync(MESH, UP_TO_DATE());
    expect(useToastStore.getState().notifications.some(needsAttention)).toBe(false);
  });

  it('resolves an earlier failure once a later sync succeeds', () => {
    reportMeshSync(MESH, FAILED('Fetch failed: network down'));
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);

    reportMeshSync(MESH, SYNCED('Pulled 1 new commit'));
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(0);
    // The problem's record survives as history, marked resolved — it is not
    // deleted, so the user can still see what happened. (The successful sync's
    // own toast is a separate, action-less entry at the top of the history.)
    const resolved = useToastStore.getState().notifications.find(n => n.action?.kind === 'mesh-sync-failure');
    expect(resolved?.action).toMatchObject({ resolution: 'Synced successfully.', detail: 'Fetch failed: network down' });
  });

  it('does not resolve another mesh\'s failure', () => {
    reportMeshSync(MESH, FAILED('Fetch failed: network down'));
    reportMeshSync({ name: 'other', path: '/repos/other' }, SYNCED('Pulled 1 new commit'));
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  });

  it('reports a thrown IPC as a failure carrying the thrown reason', () => {
    const feedback = reportSyncError(MESH, new Error('failed to open repo'));
    expect(feedback).toEqual({ severity: 'error', unresolved: true, verified: false });
    const [notice] = useToastStore.getState().notifications;
    expect(notice.action).toMatchObject({ kind: 'mesh-sync-failure', detail: 'failed to open repo' });
  });

  it('does not let a sync that checked nothing resolve an earlier failure', () => {
    // Neither mesh was compared, so the earlier failure is still the current
    // truth. Clearing it here would silently drop the user's one real problem.
    reportMeshSync(MESH, FAILED('Fetch failed: network down'));
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);

    for (const shape of [NO_REMOTE, NO_UPSTREAM]) {
      reportMeshSync(MESH, shape());
      expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
    }
  });

  it('publishes a completion only for a verified sync', () => {
    const revisions = () => useMeshSyncCompletion.getState().completions[MESH.path]?.revision ?? 0;
    expect(revisions()).toBe(0);

    reportSyncError(MESH, new Error('boom'));
    reportMeshSync(MESH, NO_REMOTE());
    expect(revisions()).toBe(0);

    reportMeshSync(MESH, SYNCED('Pulled 1 new commit'));
    expect(revisions()).toBe(1);
    reportMeshSync(MESH, UP_TO_DATE());
    // A counter, not a boolean: a repeat must still wake a subscriber up.
    expect(revisions()).toBe(2);
  });

  it('publishes a fetch without a verification for a partial sync', () => {
    // A partial run fetches but does not fast-forward, so the remote-tracking
    // ref the behind-count is measured against has moved and every row for this
    // mesh must re-read it — while the failure itself stays unresolved.
    const entry = () => useMeshSyncCompletion.getState().completions[MESH.path];
    reportMeshSync(MESH, PARTIAL());
    expect(entry()).toMatchObject({ verified: false, refsFetched: true });
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  });

  it('publishes no completion for a result that neither verified nor fetched', () => {
    // Publishing nothing and raising a card are separate questions, and this
    // pins the difference so the documented contract cannot drift from it. A
    // failed fetch is a completed `git_sync` result — the backend answered —
    // that moved no refs and established nothing, so no row's cached state
    // changes.
    reportMeshSync(MESH, FAILED());
    expect(useMeshSyncCompletion.getState().completions[MESH.path]).toBeUndefined();
    // …but the user still has to be told, so it raises an unresolved card.
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
    // A mesh with no remote neither publishes nor raises: there is nothing to
    // report and nothing that is wrong.
    reportMeshSync(MESH, NO_REMOTE());
    expect(useMeshSyncCompletion.getState().completions[MESH.path]).toBeUndefined();
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  });

  it('publishes a fetch for a no-upstream sync — the remote was contacted', () => {
    // The refs moved, so the row's cached behind-count is stale and must be
    // re-read, even though nothing was compared and nothing resolved.
    reportMeshSync(MESH, NO_UPSTREAM());
    expect(useMeshSyncCompletion.getState().completions[MESH.path])
      .toMatchObject({ verified: false, refsFetched: true });
  });

  it('reads the fetch signal from the backend flag, not the status', () => {
    // Two non-verified outcomes disagree about `fetched`: a partial and a
    // no-upstream run fetched, a failed fetch did not complete. Deriving the
    // signal from the status instead would miss exactly that difference.
    expect(outcomeOf(PARTIAL())).toEqual({ verified: false, refsFetched: true });
    expect(outcomeOf(NO_UPSTREAM())).toEqual({ verified: false, refsFetched: true });
    expect(outcomeOf(FAILED())).toEqual({ verified: false, refsFetched: false });
    expect(outcomeOf(SYNCED())).toEqual({ verified: true, refsFetched: true });
  });

  it('publishes a fetch for an unverified comparison so the row re-reads its count', () => {
    // `Unverified` fetched — the remote-tracking refs DID move — so the row's
    // cached behind-count is stale and must be re-read, even though nothing
    // was verified and nothing was resolved.
    expect(outcomeOf(UNVERIFIED())).toEqual({ verified: false, refsFetched: true });
    reportMeshSync(MESH, UNVERIFIED());
    expect(useMeshSyncCompletion.getState().completions[MESH.path])
      .toMatchObject({ verified: false, refsFetched: true });
  });
});

describe('runMeshSync', () => {
  it('invokes git_sync and reports the result it gets back', async () => {
    vi.mocked(invoke).mockResolvedValue(PARTIAL('Fetched 2 new commits; fast-forward skipped: changes to a.txt would be overwritten'));
    const { feedback, answered } = await runMeshSync(MESH);
    expect(invoke).toHaveBeenCalledWith('git_sync', { path: MESH.path });
    // `answered` means only that the backend replied — it does NOT mean the
    // fetch ran. The fetch fact comes from the result's own `fetched` flag
    // (via `outcomeOf`), which is what makes the row's refresh precise.
    expect(answered).toBe(true);
    expect(feedback.unresolved).toBe(true);
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  });

  it('reports a rejected IPC as unanswered so the caller skips the refresh', async () => {
    vi.mocked(invoke).mockRejectedValue(new Error('boom'));
    const { feedback, answered } = await runMeshSync(MESH);
    expect(answered).toBe(false);
    expect(feedback).toEqual({ severity: 'error', unresolved: true, verified: false });
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
    // A rejected IPC learned nothing about the fetch, so the card must not
    // claim it did not run.
    expect(useToastStore.getState().notifications[0].action).toMatchObject({ evidence: 'unknown' });
  });

  it('treats an unknown status as a failure instead of crashing the sidebar', async () => {
    // The wire type is generated and drift-gated, so an unknown status can
    // only come from an app/backend version mismatch. Reporting it as a
    // failure is the safe reading; throwing would take the sidebar row down
    // and leave the user with no feedback at all.
    vi.mocked(invoke).mockResolvedValue({ fetched: true, pulled: false, new_commits: 0, message: 'who knows', status: 'invented' } as unknown as GitSyncResult);
    const { feedback, answered } = await runMeshSync(MESH);
    expect(feedback).toEqual({ severity: 'error', unresolved: true, verified: false });
    expect(answered).toBe(true);
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  });
});

describe('raiseSyncAttention', () => {
  it('reuses the existing unresolved card for the same mesh instead of stacking duplicates', () => {
    raiseSyncAttention(MESH, FAILED('Fetch failed: network down'));
    raiseSyncAttention(MESH, FAILED('Fetch failed: still down'));
    const attention = useToastStore.getState().notifications.filter(needsAttention);
    expect(attention).toHaveLength(1);
    expect(attention[0].action).toMatchObject({ detail: 'Fetch failed: still down' });
  });

  it('records the problem without a visible toast when asked to be silent', () => {
    // The bulk command shows ONE summary toast; the per-mesh problems exist in
    // Needs attention only, or a dozen of them would overflow the toast stack.
    raiseSyncAttention(MESH, FAILED(), { silent: true });
    expect(useToastStore.getState().toasts).toHaveLength(0);
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  });

  it('claims lag only where it was established', () => {
    // A partial run fetched and counted, so upstream state IS known and the
    // card may say the mesh is behind.
    raiseSyncAttention(MESH, PARTIAL());
    // A failed fetch was ATTEMPTED and did not complete — `git fetch` had
    // already been launched and may have contacted upstream — so it supports
    // no claim at all, not even "nothing was fetched".
    raiseSyncAttention({ name: 'other', path: '/repos/other' }, FAILED());
    const byName = new Map(useToastStore.getState().notifications
      .filter(needsAttention).map(n => [n.action?.kind === 'mesh-sync-failure' ? n.action.meshName : '', n.action]));
    expect(byName.get(MESH.name)).toMatchObject({ evidence: 'behind' });
    expect(byName.get('other')).toMatchObject({ evidence: 'unknown' });
  });

  it('records unknown evidence for a fetch whose comparison failed', () => {
    // The backend fetched, so refs moved, but it never established where this
    // mesh stands. Neither "behind" nor "did not fetch" is supported.
    raiseSyncAttention(MESH, UNVERIFIED());
    expect(useToastStore.getState().notifications[0].action)
      .toMatchObject({ evidence: 'unknown' });
  });

  it('never claims lag for a status this build does not recognise', () => {
    const invented = { ...FAILED(), status: 'invented' } as unknown as GitSyncResult;
    raiseSyncAttention(MESH, invented);
    expect(useToastStore.getState().notifications[0].action)
      .toMatchObject({ evidence: 'unknown' });
  });

  it('still shows a toast when a silent record has no action to file it under', () => {
    // Silently filing a record nothing can surface would be a bug, not a
    // shortcut — so the flag is ignored without an action.
    addToast(SYNC_PROVIDER, 'mesh-a: something happened', 'info', undefined, { silent: true });
    expect(useToastStore.getState().toasts).toHaveLength(1);
  });
});