import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { gitSyncAllMeshes } from '../../src/components/CommandOmnibar/omnibarActions';
import { needsAttention } from '../../src/lib/notifications';
import { SYNC_PROVIDER } from '../../src/lib/meshSyncFeedback';
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
    // One summary toast, not one per mesh: with a three-toast cap, per-mesh
    // toasts would hide most of a long list. Every mesh is behind, so the
    // summary says so outright rather than implying some got through.
    const summary = useToastStore.getState().toasts.filter(t => !t.message.startsWith('a:') && !t.message.startsWith('b:'));
    expect(summary).toHaveLength(1);
    expect(summary[0].message).toBe('No mesh synced: a, b');
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