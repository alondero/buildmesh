import { describe, it, expect, vi } from 'vitest';
import {
  createOptimisticFieldLedger,
  withOptimistic,
  type AgentNodeField,
  type OptimisticFieldLedger,
  type OptimisticSurface,
} from '../../src/lib/optimistic';
import type { AgentNode } from '../../src/types/generated/AgentNode';

/**
 * Unit tests for the `withOptimistic` helper and its field-ownership
 * ledger (issues #1054, #2325).
 *
 * Three call sites in `agentNodeStore.ts` —
 * `renameAgentNode`, `setNodePinned`, `toggleNodePinned` — used to
 * hand-roll the same shape. The helper factors out the prior-capture,
 * optimistic patch, ownership claim, scoped reconciliation, and
 * `state.error` write. These tests pin each invariant separately so a
 * regression in one call site can't mask a behaviour change in another.
 *
 * The integration coverage (three sites wired through the actual
 * store, including the snapshot fence) lives in
 * `tests/unit/agent-node-store.test.ts`. Here the helper is exercised
 * against a fake `OptimisticSurface` plus the real ledger — not a
 * stand-in: the ownership bugs in #2325 live entirely in the
 * interaction between the two. Single-operation cases read as one
 * promise, one outcome; the overlapping cases drive two deferred
 * promises and settle them in a stated order rather than sleeping.
 *
 * Issue #1384 — the surface operates on a single node at a time
 * (`getAgentNode` + `setAgentNode`) rather than the whole array, which
 * is what makes the helper composable with the normalized `nodesById`
 * map.
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
  /** Stand in for the store's `patchAgentNode`: an event observation is
   *  the newest write for the columns it carries, so it releases them
   *  from the ledger before writing (issue #2325). */
  __deliverObservation: (nodeId: number, patch: Partial<AgentNode>) => void;
  /** Stand in for `removeAgentNode` / the optimistic close drop. */
  __removeNode: (nodeId: number) => void;
}

function makeSurface(initial: AgentNode[], ledger = createOptimisticFieldLedger()): {
  surface: FakeSurface;
  ledger: OptimisticFieldLedger;
} {
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
    __deliverObservation: (nodeId, patch) => {
      ledger.release(nodeId, Object.keys(patch) as AgentNodeField[]);
      fake.setAgentNode(nodeId, prev => ({ ...prev, ...patch }));
    },
    __removeNode: (nodeId) => {
      ledger.forget(nodeId);
      nodes = nodes.filter(n => n.id !== nodeId);
      fake.__lastNodes = nodes;
    },
  };
  return { surface: fake, ledger };
}

/** A promise whose settlement the test controls, so ordering is explicit
 *  rather than a sleep. */
function deferred<T>(): { promise: Promise<T>; resolve: (v: T) => void; reject: (e: unknown) => void } {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => { resolve = res; reject = rej; });
  return { promise, resolve, reject };
}

describe('withOptimistic', () => {
  it('rejects before invoking the mutation when the node is not loaded', async () => {
    const { surface, ledger } = makeSurface([]);
    const mutation = vi.fn().mockResolvedValue(undefined);

    await expect(
      withOptimistic({
        surface,
        ledger,
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
    const { surface, ledger } = makeSurface([makeNode({ id: 1, name: 'old' })]);
    // A never-settling promise lets us observe the optimistic window
    // directly. Matches the `toggleNodePinned` test in
    // agent-node-store.test.ts.
    const mutation = vi.fn().mockReturnValue(new Promise(() => {}));

    void withOptimistic({
      surface,
      ledger,
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
    const { surface, ledger } = makeSurface([makeNode({ id: 1, name: 'old' })]);

    const result = await withOptimistic({
      surface,
      ledger,
      nodeId: 1,
      optimisticPatch: { name: 'new' },
      mutation: () => Promise.resolve(undefined),
    });

    expect(result).toBeUndefined();
    // One write: optimistic patch. The acknowledged value IS the
    // optimistic one for a void-returning rename, so reconciliation has
    // nothing to change and the row keeps its identity.
    expect(surface.__setAgentNodeCalls).toBe(1);
    expect(surface.__lastNodes[0].name).toBe('new');
    expect(surface.__errorWrites).toEqual([]);
  });

  // Issue #2325 — the mutation owns its column, not the row. A pin
  // response is authoritative for `is_pinned` and for nothing else:
  // its other columns were read when the backend built the response and
  // are routinely older than observations that landed while the IPC was
  // in flight.
  it('reconciles only the claimed column from the adopted row', async () => {
    const { surface, ledger } = makeSurface([makeNode({ id: 1, name: 'old', status: 'idle' })]);
    const adopted = makeNode({ id: 1, name: 'backend-name', status: 'awaiting_input', is_pinned: true });

    await withOptimistic({
      surface,
      ledger,
      nodeId: 1,
      optimisticPatch: { is_pinned: true },
      mutation: () => Promise.resolve(adopted),
      adoptResult: (r) => r,
    });

    // The pin column follows the backend row...
    expect(surface.__lastNodes[0].is_pinned).toBe(true);
    // ...and the columns the pin never touched keep their current values
    // instead of reverting to the response's older snapshot.
    expect(surface.__lastNodes[0].name).toBe('old');
    expect(surface.__lastNodes[0].status).toBe('idle');
    expect(surface.__errorWrites).toEqual([]);
  });

  it('skips adoption when adoptResult returns undefined (mutation returned void or non-node)', async () => {
    const { surface, ledger } = makeSurface([makeNode({ id: 1, name: 'old' })]);

    await withOptimistic({
      surface,
      ledger,
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
    const { surface, ledger } = makeSurface([makeNode({ id: 1, name: 'old' })]);

    await expect(
      withOptimistic({
        surface,
        ledger,
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
    const { surface, ledger } = makeSurface([makeNode({ id: 1, name: 'old', is_pinned: false, status: 'idle' })]);

    await expect(
      withOptimistic({
        surface,
        ledger,
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
    const { surface, ledger } = makeSurface([makeNode({ id: 1, is_pinned: false, status: 'idle' })]);

    const promise = withOptimistic({
      surface,
      ledger,
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
    const { surface, ledger } = makeSurface([makeNode({ id: 1, name: 'old', is_pinned: false })]);

    await expect(
      withOptimistic({
        surface,
        ledger,
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
    const { surface, ledger } = makeSurface([makeNode({ id: 1, name: 'old' })]);
    const adoptResult = vi.fn().mockReturnValue(makeNode({ id: 1, name: 'adopted' }));

    await expect(
      withOptimistic({
        surface,
        ledger,
        nodeId: 1,
        optimisticPatch: { name: 'new' },
        mutation: () => Promise.reject(new Error('rejected')),
        adoptResult,
      }),
    ).rejects.toThrow('rejected');

    expect(adoptResult).not.toHaveBeenCalled();
  });

  it('forwards the mutation result to the caller', async () => {
    const { surface, ledger } = makeSurface([makeNode({ id: 1 })]);
    const sentinel = Symbol('mutation-result');

    const result = await withOptimistic({
      surface,
      ledger,
      nodeId: 1,
      optimisticPatch: { name: 'x' },
      mutation: () => Promise.resolve(sentinel),
    });

    expect(result).toBe(sentinel);
  });

  it('only touches the targeted node — siblings are left untouched', async () => {
    const { surface, ledger } = makeSurface([
      makeNode({ id: 1, name: 'alpha' }),
      makeNode({ id: 2, name: 'beta' }),
      makeNode({ id: 3, name: 'gamma' }),
    ]);

    await withOptimistic({
      surface,
      ledger,
      nodeId: 2,
      optimisticPatch: { name: 'beta-2' },
      mutation: () => Promise.resolve(undefined),
    });

    const names = surface.__lastNodes.map(n => n.name);
    expect(names).toEqual(['alpha', 'beta-2', 'gamma']);
  });

  // ------------------------------------------------------------------
  // Issue #2325 — operation ownership. Each case states the intended
  // durable/latest state of the claimed column, not merely which
  // surface calls happened.
  // ------------------------------------------------------------------

  it('keeps a newer rename and lifecycle observation through a successful pin (#2325)', async () => {
    // The reported reproduction: pin starts on
    // `{name: 'old', status: 'running', is_pinned: false}`; a rename and
    // a lifecycle observation land while the pin IPC is in flight; the
    // pin's row snapshot — which still carries the old name and status —
    // then resolves.
    const { surface, ledger } = makeSurface([
      makeNode({ id: 1, name: 'old', status: 'running', is_pinned: false }),
    ]);
    const pin = deferred<AgentNode>();

    const pinning = withOptimistic({
      surface,
      ledger,
      nodeId: 1,
      optimisticPatch: { is_pinned: true },
      mutation: () => pin.promise,
      adoptResult: (r) => r,
    });
    await Promise.resolve();

    surface.__deliverObservation(1, { name: 'newer-rename' });
    surface.__deliverObservation(1, { status: 'awaiting_input' });
    pin.resolve(makeNode({ id: 1, name: 'old', status: 'running', is_pinned: true }));

    await pinning;

    const row = surface.__lastNodes[0];
    expect(row.is_pinned).toBe(true);      // the pin's own column is reconciled
    expect(row.name).toBe('newer-rename'); // neither observation is erased
    expect(row.status).toBe('awaiting_input');
  });

  it('does not let an older failed rename undo a newer acknowledged rename (#2325)', async () => {
    // A starts, B starts, B is acknowledged, then A fails. The final name
    // is B's acknowledged value — the rollback that used to restore A's
    // captured prior ("old") is A no longer being the owner.
    const { surface, ledger } = makeSurface([makeNode({ id: 1, name: 'old' })]);
    const first = deferred<void>();
    const second = deferred<void>();

    const a = withOptimistic({
      surface, ledger, nodeId: 1, optimisticPatch: { name: 'first' }, mutation: () => first.promise,
    });
    await Promise.resolve();
    const b = withOptimistic({
      surface, ledger, nodeId: 1, optimisticPatch: { name: 'second' }, mutation: () => second.promise,
    });
    await Promise.resolve();

    expect(surface.__lastNodes[0].name).toBe('second');
    second.resolve();
    await b;
    first.reject(new Error('name too long'));
    await expect(a).rejects.toThrow('name too long');

    expect(surface.__lastNodes[0].name).toBe('second');
    // A still reports its own failure — the caller gets the real error.
    expect(surface.__errorWrites).toEqual(['name too long']);
  });

  describe('same-column mutations in every completion order (#2325)', () => {
    /** Start two renames on the same column, settle them in `order`,
     *  and return the resulting row. */
    async function runPair(
      firstOutcome: 'resolve' | 'reject',
      secondOutcome: 'resolve' | 'reject',
      order: 'first-first' | 'second-first',
    ) {
      const { surface, ledger } = makeSurface([makeNode({ id: 1, name: 'old' })]);
      const first = deferred<void>();
      const second = deferred<void>();
      const firstRun = withOptimistic({
        surface, ledger, nodeId: 1, optimisticPatch: { name: 'first' }, mutation: () => first.promise,
      });
      await Promise.resolve();
      const secondRun = withOptimistic({
        surface, ledger, nodeId: 1, optimisticPatch: { name: 'second' }, mutation: () => second.promise,
      });
      await Promise.resolve();

      const settle = async (which: 'first' | 'second', outcome: 'resolve' | 'reject') => {
        const pending = which === 'first' ? first : second;
        const run = which === 'first' ? firstRun : secondRun;
        if (outcome === 'resolve') {
          pending.resolve();
          await run;
        } else {
          pending.reject(new Error(`${which} failed`));
          // The rejection is asserted, not swallowed: what the ledger
          // decides is which value the row keeps, not whether the caller
          // hears about the failure.
          await expect(run).rejects.toThrow(`${which} failed`);
        }
      };

      // Order is explicit — no sleeps.
      if (order === 'first-first') {
        await settle('first', firstOutcome);
        await settle('second', secondOutcome);
      } else {
        await settle('second', secondOutcome);
        await settle('first', firstOutcome);
      }
      return surface.__lastNodes[0];
    }

    it('success/success — the newest rename survives, whichever settles first', async () => {
      expect((await runPair('resolve', 'resolve', 'first-first')).name).toBe('second');
      expect((await runPair('resolve', 'resolve', 'second-first')).name).toBe('second');
    });

    it('success/failure — a failed newer rename restores the acknowledged older one', async () => {
      expect((await runPair('resolve', 'reject', 'first-first')).name).toBe('first');
      expect((await runPair('resolve', 'reject', 'second-first')).name).toBe('first');
    });

    it('failure/success — the acknowledged newer rename survives the older failure', async () => {
      expect((await runPair('reject', 'resolve', 'first-first')).name).toBe('second');
      expect((await runPair('reject', 'resolve', 'second-first')).name).toBe('second');
    });

    it('failure/failure — both attempts are withdrawn to the pre-call name', async () => {
      expect((await runPair('reject', 'reject', 'first-first')).name).toBe('old');
      expect((await runPair('reject', 'reject', 'second-first')).name).toBe('old');
    });
  });

  it('lets a delivered event keep a column an in-flight mutation still owns (#2325)', async () => {
    // Another window renames the node while this window's rename is in
    // flight. The event is the newer observation of `name`, so the local
    // failure must not restore this window's stale prior over it.
    const { surface, ledger } = makeSurface([makeNode({ id: 1, name: 'old' })]);
    const rename = deferred<void>();
    const running = withOptimistic({
      surface, ledger, nodeId: 1, optimisticPatch: { name: 'local' }, mutation: () => rename.promise,
    });
    await Promise.resolve();

    surface.__deliverObservation(1, { name: 'from-other-window' });
    rename.reject(new Error('db locked'));
    await expect(running).rejects.toThrow('db locked');

    expect(surface.__lastNodes[0].name).toBe('from-other-window');
    expect(surface.__errorWrites).toEqual(['db locked']);
  });

  it('leaves an event-owned column alone but still reconciles the ones the mutation owns (#2325)', async () => {
    const { surface, ledger } = makeSurface([makeNode({ id: 1, name: 'local', is_pinned: false })]);
    const pin = deferred<AgentNode>();
    const pinning = withOptimistic({
      surface,
      ledger,
      nodeId: 1,
      optimisticPatch: { is_pinned: true },
      mutation: () => pin.promise,
      adoptResult: (r) => r,
    });
    await Promise.resolve();

    // A rename event supersedes `name` while the pin IPC is in flight.
    surface.__deliverObservation(1, { name: 'renamed-elsewhere' });
    pin.resolve(makeNode({ id: 1, name: 'renamed-elsewhere', is_pinned: true }));

    await pinning;

    expect(surface.__lastNodes[0].is_pinned).toBe(true);
    expect(surface.__lastNodes[0].name).toBe('renamed-elsewhere');
  });

  it('cannot resurrect a node deleted while a mutation was pending (#2325)', async () => {
    const { surface, ledger } = makeSurface([makeNode({ id: 1, name: 'old', is_pinned: false })]);
    const pin = deferred<AgentNode>();
    const pinning = withOptimistic({
      surface,
      ledger,
      nodeId: 1,
      optimisticPatch: { is_pinned: true },
      mutation: () => pin.promise,
      adoptResult: (r) => r,
    });
    await Promise.resolve();
    expect(surface.__lastNodes[0].is_pinned).toBe(true);

    surface.__removeNode(1);
    const writesBefore = surface.__setAgentNodeCalls;
    pin.resolve(makeNode({ id: 1, name: 'old', status: 'idle', is_pinned: true }));
    await pinning;

    expect(surface.__lastNodes).toHaveLength(0);
    expect(surface.__setAgentNodeCalls).toBe(writesBefore);
  });

  it('reports every start and settlement to the owner as a field write (#2325)', async () => {
    // The store uses this to fence full reads: a `list_agent_nodes`
    // snapshot that began before the mutation must not be painted over
    // the columns the mutation owns, even when reconciliation changes
    // nothing visible.
    const onFieldWrite = vi.fn();
    const ledger = createOptimisticFieldLedger({ onFieldWrite });
    const { surface } = makeSurface([makeNode({ id: 1, name: 'old' })], ledger);

    await withOptimistic({
      surface,
      ledger,
      nodeId: 1,
      optimisticPatch: { name: 'new' },
      mutation: () => Promise.resolve(undefined),
    });

    expect(onFieldWrite.mock.calls).toEqual([[1], [1]]);
  });
});