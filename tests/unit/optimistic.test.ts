import { describe, it, expect, vi } from 'vitest';
import { withOptimistic, type OptimisticSurface } from '../../src/lib/optimistic';
import type { AgentNode } from '../../src/types/generated/AgentNode';

/**
 * Unit tests for the `withOptimistic` helper (issue #1054).
 *
 * Three call sites in `agentNodeStore.ts` —
 * `renameAgentNode`, `setNodePinned`, `toggleNodePinned` — used to
 * hand-roll the same shape. The helper factors out the prior-capture,
 * optimistic patch, narrow rollback, optional adopt-on-success, and
 * `state.error` write. These tests pin each invariant separately so a
 * regression in one call site can't mask a behaviour change in another.
 *
 * The integration coverage (three sites wired through the actual
 * store) lives in `tests/unit/agent-node-store.test.ts`. Here the
 * helper is exercised in isolation against a fake `OptimisticSurface`
 * — every test reads as one promise, one outcome.
 *
 * Issue #1384 — the surface now operates on a single node at a time
 * (`getAgentNode` + `setAgentNode`) rather than the whole array. The
 * per-node scope is what makes the helper composable with the
 * normalized `nodesById` map; a future Zustand-style entity adapter
 * could swap the fake for the real store without changing this file.
 */

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 1, mesh_id: 1, name: 'old', path: '/a', branch: 'main',
    env: 'windows', provider: 'anthropic', status: 'idle', created_at: '',
    use_worktree: true, position: 0,
    is_pinned: false,
    ...overrides,
  };
}

interface FakeSurface extends OptimisticSurface {
  __setAgentNodeCalls: number;
  __errorWrites: Array<string | null>;
  __lastNodes: AgentNode[];
}

function makeSurface(initial: AgentNode[]): FakeSurface {
  let nodes: AgentNode[] = initial;
  const fake: FakeSurface = {
    getAgentNode: (nodeId) => nodes.find(n => n.id === nodeId),
    setAgentNode: (nodeId, next) => {
      fake.__setAgentNodeCalls += 1;
      nodes = nodes.map(n => (n.id === nodeId ? next(n) : n));
      fake.__lastNodes = nodes;
    },
    setError: (error) => {
      fake.__errorWrites.push(error);
    },
    __setAgentNodeCalls: 0,
    __errorWrites: [],
    __lastNodes: nodes,
  };
  return fake;
}

describe('withOptimistic', () => {
  it('rejects before invoking the mutation when the node is not loaded', async () => {
    const surface = makeSurface([]);
    const mutation = vi.fn().mockResolvedValue(undefined);

    await expect(
      withOptimistic({
        surface,
        nodeId: 999,
        optimisticPatch: { name: 'new' },
        mutation,
      }),
    ).rejects.toThrow(/not loaded/);

    expect(mutation).not.toHaveBeenCalled();
    // No patch, no error — the precondition guard fired before either
    // side effect could run.
    expect(surface.__setAgentNodeCalls).toBe(0);
    expect(surface.__errorWrites).toEqual([]);
  });

  it('applies the optimistic patch synchronously, before awaiting the mutation', () => {
    const surface = makeSurface([makeNode({ id: 1, name: 'old' })]);
    // A never-settling promise lets us observe the optimistic window
    // directly. Matches the `toggleNodePinned` test in
    // agent-node-store.test.ts.
    const mutation = vi.fn().mockReturnValue(new Promise(() => {}));

    void withOptimistic({
      surface,
      nodeId: 1,
      optimisticPatch: { name: 'new' },
      mutation,
    });

    // One setAgentNode call (the optimistic patch), no error writes.
    expect(surface.__setAgentNodeCalls).toBe(1);
    expect(surface.__lastNodes[0].name).toBe('new');
    expect(surface.__errorWrites).toEqual([]);
  });

  it('leaves the optimistic patch in place on success when adoptResult is omitted', async () => {
    const surface = makeSurface([makeNode({ id: 1, name: 'old' })]);

    const result = await withOptimistic({
      surface,
      nodeId: 1,
      optimisticPatch: { name: 'new' },
      mutation: () => Promise.resolve(undefined),
    });

    expect(result).toBeUndefined();
    // One write: optimistic patch. No adoption (adoptResult omitted),
    // so the helper does not call setAgentNode a second time.
    expect(surface.__setAgentNodeCalls).toBe(1);
    expect(surface.__lastNodes[0].name).toBe('new');
    expect(surface.__errorWrites).toEqual([]);
  });

  it('replaces the whole row via adoptResult when the mutation returns an AgentNode', async () => {
    const surface = makeSurface([makeNode({ id: 1, name: 'old', status: 'idle' })]);
    const adopted = makeNode({ id: 1, name: 'backend-name', status: 'awaiting_input', is_pinned: true });

    await withOptimistic({
      surface,
      nodeId: 1,
      optimisticPatch: { is_pinned: true },
      mutation: () => Promise.resolve(adopted),
      adoptResult: (r) => r,
    });

    // Two writes: optimistic patch + adopt. The adopted row is the
    // source of truth — backend-returned columns (status, name)
    // overwrite whatever the optimistic patch set.
    expect(surface.__setAgentNodeCalls).toBe(2);
    expect(surface.__lastNodes[0]).toEqual(adopted);
    expect(surface.__errorWrites).toEqual([]);
  });

  it('skips adoption when adoptResult returns undefined (mutation returned void or non-node)', async () => {
    const surface = makeSurface([makeNode({ id: 1, name: 'old' })]);

    await withOptimistic({
      surface,
      nodeId: 1,
      optimisticPatch: { name: 'new' },
      mutation: () => Promise.resolve(undefined),
      adoptResult: () => undefined,
    });

    // One write: optimistic patch only. The helper short-circuits the
    // adoption when `adoptResult` returns undefined — same outcome as
    // the "adoptResult omitted" case.
    expect(surface.__setAgentNodeCalls).toBe(1);
    expect(surface.__lastNodes[0].name).toBe('new');
  });

  it('rolls back the patched column on rejection and writes state.error', async () => {
    const surface = makeSurface([makeNode({ id: 1, name: 'old' })]);

    await expect(
      withOptimistic({
        surface,
        nodeId: 1,
        optimisticPatch: { name: 'new' },
        mutation: () => Promise.reject(new Error('db locked')),
      }),
    ).rejects.toThrow('db locked');

    // Two writes: optimistic patch + rollback. The final name is the
    // pre-call value.
    expect(surface.__setAgentNodeCalls).toBe(2);
    expect(surface.__lastNodes[0].name).toBe('old');
    // The error reaches the store via `surface.setError`.
    expect(surface.__errorWrites).toEqual(['db locked']);
  });

  // The narrow-rollback invariant that the existing
  // `toggleNodePinned rolls back ONLY is_pinned on rejection` test
  // pins at the store level — the helper must not clobber a
  // concurrent write to an unrelated column.
  it('rolls back ONLY the keys in optimisticPatch, preserving concurrent writes to other columns', async () => {
    const surface = makeSurface([makeNode({ id: 1, name: 'old', is_pinned: false, status: 'idle' })]);

    await expect(
      withOptimistic({
        surface,
        nodeId: 1,
        optimisticPatch: { is_pinned: true },
        mutation: () => Promise.reject(new Error('ipc disconnected')),
      }),
    ).rejects.toThrow('ipc disconnected');

    const row = surface.__lastNodes[0];
    expect(row.is_pinned).toBe(false); // rolled back
    expect(row.name).toBe('old'); // untouched
    expect(row.status).toBe('idle'); // untouched — would be 'awaiting_input' if a concurrent write had fired
  });

  it('preserves a concurrent write to a non-patched column on the rejection path', async () => {
    // Simulates the agent-orchestrator flipping `status` to
    // 'awaiting_input' between the optimistic patch and the
    // rejection. The post-rejection row must keep the new status —
    // a wide-rollback (full prior) would clobber it.
    const surface = makeSurface([makeNode({ id: 1, is_pinned: false, status: 'idle' })]);

    const promise = withOptimistic({
      surface,
      nodeId: 1,
      optimisticPatch: { is_pinned: true },
      mutation: () => new Promise<never>((_, reject) => {
        // Concurrent event-driven write fires while we're awaiting.
        // Issue #1384 — the surface now operates per-node; simulate a
        // concurrent status flip on the same id via the same setter.
        surface.setAgentNode(1, (prev) => ({ ...prev, status: 'awaiting_input' as const }));
        setTimeout(() => reject(new Error('db locked')), 0);
      }),
    });

    await expect(promise).rejects.toThrow('db locked');

    const row = surface.__lastNodes[0];
    expect(row.is_pinned).toBe(false); // rolled back
    expect(row.status).toBe('awaiting_input'); // concurrent write preserved
  });

  it('rolls back multiple keys when optimisticPatch covers more than one column', async () => {
    const surface = makeSurface([makeNode({ id: 1, name: 'old', is_pinned: false })]);

    await expect(
      withOptimistic({
        surface,
        nodeId: 1,
        optimisticPatch: { name: 'new', is_pinned: true },
        mutation: () => Promise.reject(new Error('ipc failed')),
      }),
    ).rejects.toThrow('ipc failed');

    const row = surface.__lastNodes[0];
    expect(row.name).toBe('old');
    expect(row.is_pinned).toBe(false);
  });

  it('does not call adoptResult when the mutation rejects', async () => {
    const surface = makeSurface([makeNode({ id: 1, name: 'old' })]);
    const adoptResult = vi.fn().mockReturnValue(makeNode({ id: 1, name: 'adopted' }));

    await expect(
      withOptimistic({
        surface,
        nodeId: 1,
        optimisticPatch: { name: 'new' },
        mutation: () => Promise.reject(new Error('rejected')),
        adoptResult,
      }),
    ).rejects.toThrow('rejected');

    expect(adoptResult).not.toHaveBeenCalled();
  });

  it('forwards the mutation result to the caller', async () => {
    const surface = makeSurface([makeNode({ id: 1 })]);
    const sentinel = Symbol('mutation-result');

    const result = await withOptimistic({
      surface,
      nodeId: 1,
      optimisticPatch: { name: 'x' },
      mutation: () => Promise.resolve(sentinel),
    });

    expect(result).toBe(sentinel);
  });

  it('only touches the targeted node — siblings are left untouched', async () => {
    const surface = makeSurface([
      makeNode({ id: 1, name: 'alpha' }),
      makeNode({ id: 2, name: 'beta' }),
      makeNode({ id: 3, name: 'gamma' }),
    ]);

    await withOptimistic({
      surface,
      nodeId: 2,
      optimisticPatch: { name: 'beta-2' },
      mutation: () => Promise.resolve(undefined),
    });

    const names = surface.__lastNodes.map(n => n.name);
    expect(names).toEqual(['alpha', 'beta-2', 'gamma']);
  });
});
