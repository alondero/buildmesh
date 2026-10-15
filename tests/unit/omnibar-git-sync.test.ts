import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { gitSyncAllMeshes } from '../../src/components/CommandOmnibar/omnibarActions';
import { needsAttention } from '../../src/lib/notifications';
import { SYNC_PROVIDER, useMeshSyncCompletion } from '../../src/lib/meshSyncFeedback';
import { useToastStore } from '../../src/stores/toastStore';
import type { GitSyncResult } from '../../src/types/generated/GitSyncResult';
import type { Mesh } from '../../src/types/generated/Mesh';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

function mesh(id: number, name: string): Mesh {
  return { id, name, path: `/repos/${name}`, layout: 'single', position: 0, created_at: '2026-01-01', base_ref: 'main' };
}

function sync(message: string, overrides: Partial<GitSyncResult> & Pick<GitSyncResult, 'status'>): GitSyncResult {
  return { fetched: true, pulled: false, new_commits: 0, message, ...overrides };
}

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  useToastStore.setState({ toasts: [], notifications: [], worktreeRevision: 0, cleanupError: null, cleanupLoading: false });
  useMeshSyncCompletion.setState({ completions: {} });
});

describe('omnibar Git sync across meshes', () => {
  it('confirms the whole run when every mesh syncs', async () => {
    vi.mocked(invoke).mockResolvedValue(sync('Already up to date', { status: 'up_to_date' }));
    await gitSyncAllMeshes([mesh(1, 'a'), mesh(2, 'b')]);

    const toasts = useToastStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toMatchObject({ provider: SYNC_PROVIDER, severity: 'success' });
    expect(toasts[0].message).toBe('Synced 2 meshes');
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(0);
  });

  it('counts a fetched-but-not-fast-forwarded mesh as a problem, not a success', async () => {
    // The bulk command used to count any resolved promise as success, so a
    // mesh left behind upstream was reported as synced.
    vi.mocked(invoke).mockResolvedValue(sync('Fetched 1 new commit but fast-forward failed: diverged', { status: 'partial' }));
    await gitSyncAllMeshes([mesh(1, 'a'), mesh(2, 'b')]);

    const attention = useToastStore.getState().notifications.filter(needsAttention);
    expect(attention).toHaveLength(2);
    expect(attention[0].action).toMatchObject({ meshName: expect.stringMatching(/^[ab]$/) });
    // ONE toast for the whole run. Both meshes are behind, so the summary says
    // so outright rather than implying some got through.
    expect(useToastStore.getState().toasts).toHaveLength(1);
    expect(useToastStore.getState().toasts[0].message).toBe('No mesh synced: a, b');
  });

  it('shows one toast for the run, not one per failing mesh', async () => {
    // The failure mode this prevents: N problems each pushing a toast onto a
    // three-deep stack, so the meshes the user most needs to know about are the
    // ones whose toast has already expired.
    vi.mocked(invoke).mockResolvedValue(sync('Fetch failed: network down', { status: 'failed', fetched: false }));
    await gitSyncAllMeshes([mesh(1, 'a'), mesh(2, 'b'), mesh(3, 'c'), mesh(4, 'd')]);

    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(4);
    const toasts = useToastStore.getState().toasts;
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toMatchObject({ provider: SYNC_PROVIDER, severity: 'warning' });
    expect(toasts[0].message).toBe('No mesh synced: a, b, c, d');
  });

  it('reports a mesh with no remote as skipped rather than synced', async () => {
    // A skipped sync contacted nothing, so counting it as synced claims a
    // result the run never obtained.
    vi.mocked(invoke).mockResolvedValue(sync('No remote configured', { status: 'skipped', fetched: false }));
    await gitSyncAllMeshes([mesh(1, 'a'), mesh(2, 'b')]);

    expect(useToastStore.getState().toasts[0].message)
      .toBe('No mesh synced — no remote configured: a, b');
    expect(useToastStore.getState().toasts[0].severity).toBe('info');
  });

  it('counts skipped meshes separately when others really did sync', async () => {
    vi.mocked(invoke).mockImplementation(async (_cmd, args) => ((args as { path: string }).path === '/repos/a'
      ? sync('No remote configured', { status: 'skipped', fetched: false })
      : sync('Pulled 1 new commit', { status: 'synced', pulled: true, new_commits: 1 })));
    await gitSyncAllMeshes([mesh(1, 'a'), mesh(2, 'b')]);

    expect(useToastStore.getState().toasts[0].message)
      .toBe('Synced 1 of 2 meshes; skipped with no remote: a');
  });

  it('names skipped meshes in the problem summary too', async () => {
    // A skipped mesh raises no Needs-attention item, so a problem-only summary
    // left the no-remote outcome reported nowhere at all.
    vi.mocked(invoke).mockImplementation(async (_cmd, args) => ((args as { path: string }).path === '/repos/a'
      ? sync('Fetch failed: network down', { status: 'failed', fetched: false })
      : sync('No remote configured', { status: 'skipped', fetched: false })));
    await gitSyncAllMeshes([mesh(1, 'a'), mesh(2, 'b')]);

    expect(useToastStore.getState().toasts).toHaveLength(1);
    expect(useToastStore.getState().toasts[0].message)
      .toBe('Sync incomplete for: a; skipped (no remote): b');
    // One card for the one real problem; the no-remote mesh raises none.
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  });

  it('publishes a fetch for a mesh left partially synced', async () => {
    // The bulk run advances the upstream ref without fast-forwarding, so the
    // row's cached behind-count must be re-read even though nothing resolved.
    vi.mocked(invoke).mockResolvedValue(sync('Fetched 9 new commits but fast-forward failed: diverged', { status: 'partial' }));
    await gitSyncAllMeshes([mesh(1, 'a')]);

    expect(useMeshSyncCompletion.getState().completions['/repos/a'])
      .toMatchObject({ verified: false, refsFetched: true });
  });

  it('publishes nothing for a mesh that only skipped', async () => {
    vi.mocked(invoke).mockResolvedValue(sync('No remote configured', { status: 'skipped', fetched: false }));
    await gitSyncAllMeshes([mesh(1, 'a')]);

    expect(useMeshSyncCompletion.getState().completions['/repos/a']).toBeUndefined();
  });

  it('does not let a skipped mesh resolve an earlier real failure', async () => {
    vi.mocked(invoke).mockResolvedValue(sync('Fetch failed: network down', { status: 'failed', fetched: false }));
    await gitSyncAllMeshes([mesh(1, 'a')]);
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);

    vi.mocked(invoke).mockResolvedValue(sync('No remote configured', { status: 'skipped', fetched: false }));
    await gitSyncAllMeshes([mesh(1, 'a')]);
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);
  });

  it('keeps going after one mesh throws and reports it as a failure', async () => {
    vi.mocked(invoke).mockImplementation(async (_cmd, args) => {
      if ((args as { path: string })?.path === '/repos/a') throw new Error('command failed');
      return sync('Pulled 1 new commit', { status: 'synced', pulled: true, new_commits: 1 });
    });
    await gitSyncAllMeshes([mesh(1, 'a'), mesh(2, 'b')]);

    // Both meshes were attempted: the throw must not abort the rest. Counted
    // per command because the IPC chokepoint also logs the rejection.
    const syncs = vi.mocked(invoke).mock.calls.filter(c => c[0] === 'git_sync');
    expect(syncs).toHaveLength(2);
    const attention = useToastStore.getState().notifications.filter(needsAttention);
    expect(attention).toHaveLength(1);
    expect(attention[0].action).toMatchObject({ kind: 'mesh-sync-failure', meshName: 'a', detail: 'command failed' });
  });

  it('says nothing when there is no mesh to sync', async () => {
    await gitSyncAllMeshes([]);
    expect(invoke).not.toHaveBeenCalled();
    expect(useToastStore.getState().toasts).toHaveLength(0);
  });

  it('says nothing synced when every mesh failed', async () => {
    vi.mocked(invoke).mockResolvedValue(sync('Fetch failed: network down', { status: 'failed', fetched: false }));
    await gitSyncAllMeshes([mesh(1, 'a')]);

    expect(useToastStore.getState().toasts.at(-1)?.message).toBe('No mesh synced: a');
  });

  it('resolves a mesh that previously failed once it syncs cleanly', async () => {
    vi.mocked(invoke).mockResolvedValue(sync('Fetch failed: network down', { status: 'failed', fetched: false }));
    await gitSyncAllMeshes([mesh(1, 'a')]);
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(1);

    vi.mocked(invoke).mockResolvedValue(sync('Pulled 1 new commit', { status: 'synced', pulled: true, new_commits: 1 }));
    await gitSyncAllMeshes([mesh(1, 'a')]);
    expect(useToastStore.getState().notifications.filter(needsAttention)).toHaveLength(0);
  });
});