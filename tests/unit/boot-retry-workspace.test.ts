/**
 * Rank 5 of issue #2024 — startup reads must reject explicitly.
 *
 * Issue #2024 listed this as still-open against `meshStore.fetchMeshes`
 * (which calls the absorbing `loadMeshes(false)`) and
 * `agentNodeStore.fetchAgentNodes` (which catches without rejecting), and
 * asked for: *"Reject each primary read independently, then recover on Retry
 * and assert the correct workspace loads."*
 *
 * The verdict half of that contract is already pinned by
 * `boot-sequence.test.ts` against the coordinator. What is NOT pinned there
 * is the workspace half: that a successful retry actually puts the right
 * meshes and nodes into the real stores, rather than merely clearing the
 * error and painting an empty grid.
 *
 * These tests drive `runBoot` with the REAL store actions
 * (`useMeshStore.refreshMeshes`, `useAgentNodeStore.fetchAgentNodes`) and a
 * fake Tauri IPC, so the absorbing catch, the store error, and the commit
 * are all exercised as production code.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { runBoot, type BootSnapshotErrors } from '../../src/lib/bootSequence';
import { useMeshStore } from '../../src/stores/meshStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import type { Mesh } from '../../src/types/generated/Mesh';
import type { AgentNode } from '../../src/types/generated/AgentNode';

const tauriMocks = vi.hoisted(() => ({
  listMeshes: vi.fn(),
  listAgentNodes: vi.fn(),
  listCircuitAgentOwnerships: vi.fn(),
  listSemanticTurns: vi.fn(),
}));

// Partial mock: the stores transitively pull in other `lib/tauri` exports
// (hooks, listeners), and replacing the whole module would break them.
vi.mock('../../src/lib/tauri', async (importOriginal) => ({
  ...(await importOriginal<typeof import('../../src/lib/tauri')>()),
  listMeshes: tauriMocks.listMeshes,
  listAgentNodes: tauriMocks.listAgentNodes,
  listCircuitAgentOwnerships: tauriMocks.listCircuitAgentOwnerships,
  listSemanticTurns: tauriMocks.listSemanticTurns,
}));

const MESH: Mesh = {
  id: 42,
  name: 'buildmesh',
  path: '/repos/buildmesh',
  color: null,
  base_ref: 'origin/main',
  use_worktree: true,
  created_at: '2026-06-11T00:00:00Z',
  scratchpad: '',
  sandbox: false,
};

const NODE: AgentNode = {
  id: 7,
  mesh_id: 42,
  name: 'fix-auth',
  path: '/repos/buildmesh/.claude/worktrees/fix-auth',
  branch: 'agent/fix-auth',
  env: 'windows',
  provider: 'anthropic',
  status: 'idle',
  cli_session_id: null,
  created_at: '2026-06-11T00:00:00Z',
  position: 0,
} as unknown as AgentNode;

/** The boot coordinator wired to the real stores, exactly as `App.init` does. */
function bootLoaders() {
  return {
    attachListeners: vi.fn(async () => {}),
    loadMeshes: () => useMeshStore.getState().refreshMeshes(),
    loadAgentNodes: () => useAgentNodeStore.getState().fetchAgentNodes(),
    readSnapshotErrors: (): BootSnapshotErrors => ({
      meshes: useMeshStore.getState().error,
      agentNodes: useAgentNodeStore.getState().error,
    }),
  };
}

beforeEach(() => {
  tauriMocks.listMeshes.mockReset();
  tauriMocks.listAgentNodes.mockReset();
  tauriMocks.listCircuitAgentOwnerships.mockReset().mockResolvedValue([]);
  tauriMocks.listSemanticTurns.mockReset().mockResolvedValue([]);
  useMeshStore.setState({ meshes: [], meshesById: new Map(), error: null, loading: false });
  useAgentNodeStore.setState({ nodesById: {}, nodeIds: [], error: null, loading: false });
});

describe('startup reads reject independently and recover on retry (issue #2024 rank 5)', () => {
  it('fails the boot when only the Mesh snapshot fails, while the node snapshot still loads', async () => {
    tauriMocks.listMeshes.mockRejectedValue(new Error('mesh IPC died'));
    tauriMocks.listAgentNodes.mockResolvedValue([NODE]);

    const first = await runBoot(bootLoaders());

    expect(first.ok).toBe(false);
    expect(first.errors.join('\n')).toContain('mesh IPC died');
    // The independent read still committed its own data.
    expect(useAgentNodeStore.getState().nodeIds).toEqual([7]);
  });

  it('fails the boot when only the Agent Node snapshot fails, while the mesh snapshot still loads', async () => {
    tauriMocks.listMeshes.mockResolvedValue([MESH]);
    tauriMocks.listAgentNodes.mockRejectedValue(new Error('node IPC died'));

    const first = await runBoot(bootLoaders());

    expect(first.ok).toBe(false);
    expect(first.errors.join('\n')).toContain('node IPC died');
    expect(useMeshStore.getState().meshes).toHaveLength(1);
  });

  it('recovers on Retry and loads the correct workspace', async () => {
    tauriMocks.listMeshes.mockRejectedValueOnce(new Error('mesh IPC died'));
    tauriMocks.listAgentNodes.mockRejectedValueOnce(new Error('node IPC died'));

    const failed = await runBoot(bootLoaders());
    expect(failed.ok).toBe(false);
    // The failed boot painted nothing.
    expect(useMeshStore.getState().meshes).toHaveLength(0);
    expect(useAgentNodeStore.getState().nodeIds).toHaveLength(0);

    // Retry — `BootErrorPanel`'s button re-runs this exact call.
    tauriMocks.listMeshes.mockResolvedValue([MESH]);
    tauriMocks.listAgentNodes.mockResolvedValue([NODE]);

    const retried = await runBoot(bootLoaders());

    expect(retried.ok).toBe(true);
    expect(retried.errors).toEqual([]);
    // The correct workspace is now loaded, not merely "no longer erroring".
    expect(useMeshStore.getState().meshes.map((m) => m.id)).toEqual([42]);
    expect(useMeshStore.getState().meshesById.get(42)?.name).toBe('buildmesh');
    expect(useAgentNodeStore.getState().nodeIds).toEqual([7]);
    expect(useAgentNodeStore.getState().nodesById[7]?.name).toBe('fix-auth');
  });
});