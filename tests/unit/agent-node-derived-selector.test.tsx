/**
 * Issue #2021 — derived node-array selector: unrelated-node work.
 *
 * `useAllAgentNodes` is subscribed by seven components in the real app
 * (Sidebar, AttentionList, AgentHistoryTab, CommandOmnibar, ScopeIndicator,
 * GridFilterPopover, AgentNodeView). Before #2021 every subscriber re-derived
 * the full array (`nodeIds.map(...).filter(...)`) on every store notification,
 * allocating O(N) per subscriber; profiling at 100/500/1000 nodes measured
 * 700 / 3500 / 7000 `nodesById` dereferences for a single write that changed
 * no node at all.
 *
 * The derivation is now memoized once per notification. These tests pin the
 * behaviour that must survive that change:
 *   - a write that does not touch node content costs no derivation work;
 *   - a write that DOES change a node still produces a new array (so
 *     downstream `useMemo` derivations recompute);
 *   - a fetch that reconciles to no field change keeps the array reference
 *     stable (the guarantee `useShallow` used to provide);
 *   - ordering, filtering and per-node identity are unchanged;
 *   - multiple subscribers share one derivation rather than each redoing it.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { act, render } from '@testing-library/react';
import { createElement, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
  useAgentNodeStore,
  useAllAgentNodes,
  type AgentNode,
} from '../../src/stores/agentNodeStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';

vi.mock('../../src/components/Terminal/Terminal', () => ({
  disposeTerminal: vi.fn(),
  AgentTerminal: () => null,
}));
vi.mock('../../src/stores/toastStore', () => ({ addToast: vi.fn(), dismissToast: vi.fn() }));

const mockInvoke = invoke as ReturnType<typeof vi.fn>;

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 1,
    mesh_id: 1,
    name: 'bold-keen-brook',
    path: '/a',
    branch: 'main',
    env: 'windows',
    provider: 'anthropic',
    status: 'idle',
    created_at: '2026-01-01T00:00:00Z',
    use_worktree: false,
    position: 0,
    ...overrides,
  } as AgentNode;
}

function makeNodes(count: number): AgentNode[] {
  return Array.from({ length: count }, (_, i) =>
    makeNode({ id: i + 1, mesh_id: (i % 3) + 1, position: i }),
  );
}

/**
 * Wrap `nodesById` so every `nodesById[id]` dereference the derivation makes
 * is counted. Deterministic — not a timing assertion — so it distinguishes
 * "the derivation ran once" from "the derivation ran per subscriber" without
 * depending on machine load.
 *
 * Only usable for notifications that leave `nodesById` in place. Any store
 * write that replaces the container (`patchAgentNode`, a refetch) spreads the
 * Proxy into a plain object, so reads after that point bypass the counter and
 * a count taken then would be blind to the derivation. Use reference identity
 * to assert those cases instead.
 */
function seedCounting(nodes: AgentNode[]): { reads: number } {
  const raw: Record<number, AgentNode> = {};
  const nodeIds: number[] = [];
  for (const n of nodes) {
    raw[n.id] = n;
    nodeIds.push(n.id);
  }
  const counter = { reads: 0 };
  useAgentNodeStore.setState({
    nodesById: new Proxy(raw, {
      get(target, prop, receiver) {
        counter.reads += 1;
        return Reflect.get(target, prop, receiver);
      },
    }),
    nodeIds,
    activeNodeId: null,
  });
  return counter;
}

describe('useAllAgentNodes derived selector (issue #2021)', () => {
  let renders = 0;
  /** Every `(nodes, group count)` the consumer derived, in render order. */
  let derived: { nodes: AgentNode[]; groups: number }[] = [];

  /** The most recent derived array — i.e. what the consumer currently holds. */
  function latest(): AgentNode[] {
    return derived[derived.length - 1].nodes;
  }

  function Consumer() {
    const nodes = useAllAgentNodes();
    renders += 1;
    // A realistic downstream derivation: the same O(N) grouping pass Sidebar
    // and AgentNodeView run. It must re-run exactly when the array changes,
    // which is what makes the array's reference stability observable.
    const grouped = useMemo(() => {
      const byMesh = new Map<number, AgentNode[]>();
      for (const n of nodes) {
        const list = byMesh.get(n.mesh_id);
        if (list) list.push(n);
        else byMesh.set(n.mesh_id, [n]);
      }
      return byMesh;
    }, [nodes]);
    // Record every derivation result (mount + each recomputation) so a test
    // can assert on reference identity across renders.
    derived.push({ nodes, groups: grouped.size });
    return createElement('div', null, nodes.length);
  }

  beforeEach(() => {
    renders = 0;
    derived = [];
    useAgentNodeStore.setState({
      nodesById: {},
      nodeIds: [],
      activeNodeId: null,
      circuitOwnerships: {},
      semanticTurns: {},
      loading: false,
      error: null,
      closingNodeIds: new Set(),
      stalledInputs: {},
      schedules: {},
    });
    vi.clearAllMocks();
  });

  afterEach(() => {
    vi.restoreAllMocks();
  });

  it('derives the full ordered list for a subscriber', () => {
    seedAgentNodes([makeNode({ id: 3 }), makeNode({ id: 1 }), makeNode({ id: 2 })]);
    render(createElement(Consumer));
    // Order comes from `nodeIds`, not from map insertion order.
    expect(latest().map(n => n.id)).toEqual([3, 1, 2]);
  });

  it('costs no derivation work for a write that touches no node', () => {
    const counter = seedCounting(makeNodes(500));
    render(createElement(Consumer));
    const before = renders;
    // The mount itself derives once; measure only what the write costs.
    counter.reads = 0;

    // A Circuit ownership patch: the store writes `circuitOwnerships` only.
    act(() => {
      useAgentNodeStore.setState((s) => ({
        circuitOwnerships: {
          ...s.circuitOwnerships,
          1: { node_id: 1, run_id: 3, circuit_id: 2, circuit_name: 'c', state: 'running', parent_node_id: null },
        },
      }));
    });

    expect(counter.reads).toBe(0);
    expect(renders).toBe(before);
  });

  it('derives once for all subscribers rather than once per subscriber', () => {
    const NODES = 500;
    seedCounting(makeNodes(NODES));
    // Three mounted subscribers. Each was handed its OWN array before this
    // change; they now share one derived instance.
    const views = [
      render(createElement(Consumer)),
      render(createElement(Consumer)),
      render(createElement(Consumer)),
    ];
    const rendersBefore = derived.length;

    act(() => {
      useAgentNodeStore.getState().patchAgentNode(2, { status: 'working' });
    });

    // Every subscriber re-rendered...
    expect(derived.length - rendersBefore).toBe(3);
    const afterChange = derived.slice(rendersBefore).map(d => d.nodes);
    expect(afterChange).toHaveLength(3);
    // ...and all three were handed the SAME array instance rather than three
    // equal-but-distinct ones. This is what proves the derivation ran once and
    // was shared, rather than once per subscriber.
    expect(afterChange[1]).toBe(afterChange[0]);
    expect(afterChange[2]).toBe(afterChange[0]);
    expect(afterChange[0].find(n => n.id === 2)?.status).toBe('working');

    for (const view of views) view.unmount();
  });

  it('still produces a new array when a node actually changes', () => {
    const nodes = makeNodes(10);
    seedAgentNodes(nodes);
    render(createElement(Consumer));
    const before = latest();

    act(() => {
      useAgentNodeStore.getState().patchAgentNode(4, { status: 'working' });
    });
    const after = latest();

    // Downstream `useMemo` derivations must see the change.
    expect(after).not.toBe(before);
    expect(after.find(n => n.id === 4)?.status).toBe('working');
    // Unrelated rows keep their object identity, so per-row memo still bails.
    expect(after.find(n => n.id === 5)).toBe(before.find(n => n.id === 5));
  });

  it('keeps the array reference stable when a fetch reconciles to no change', async () => {
    const nodes = makeNodes(20);
    seedAgentNodes(nodes);
    render(createElement(Consumer));
    const before = latest();

    // A `fetchAgentNodes` poll returns the same field values: the store's
    // shallow reconciliation keeps every per-id reference, so consumers must
    // keep their array reference and skip the render (the `useShallow`
    // guarantee).
    mockInvoke.mockImplementation((command: string) => {
      if (command === 'list_agent_nodes') return Promise.resolve(nodes);
      if (command === 'list_circuit_agent_ownerships') return Promise.resolve([]);
      if (command === 'list_semantic_turns') return Promise.resolve([]);
      return Promise.resolve(undefined);
    });

    await act(async () => {
      await useAgentNodeStore.getState().fetchAgentNodes();
    });
    // The array reference the consumer holds is unchanged, and no render
    // happened: the derivation is reference-stable, so the consumer never saw
    // a new array at all.
    expect(latest()).toBe(before);
    expect(derived).toHaveLength(1);
  });

  it('returns a fresh array after a delete so ordering stays correct', () => {
    const nodes = makeNodes(5);
    seedAgentNodes(nodes);
    render(createElement(Consumer));
    const before = latest();

    act(() => {
      useAgentNodeStore.setState((s) => {
        const nodesById = { ...s.nodesById };
        delete nodesById[3];
        return { nodesById, nodeIds: s.nodeIds.filter(id => id !== 3) };
      });
    });

    const after = latest();
    expect(after).not.toBe(before);
    expect(after.map(n => n.id)).toEqual([1, 2, 4, 5]);
  });

  it('preserves pinned nodes, status and mesh assignment across a regroup', () => {
    // The selection/sort/pinned/activity invariants the issue requires us to
    // preserve: a derived-list change must not alter any field of any row.
    const nodes = [
      makeNode({ id: 1, mesh_id: 1, status: 'idle', is_pinned: true, position: 2 }),
      makeNode({ id: 2, mesh_id: 2, status: 'working', is_pinned: false, position: 1 }),
      makeNode({ id: 3, mesh_id: 1, status: 'awaiting_input', is_pinned: true, position: 0 }),
    ];
    seedAgentNodes(nodes);
    render(createElement(Consumer));

    expect(latest().find(n => n.id === 1)?.is_pinned).toBe(true);
    expect(latest().find(n => n.id === 2)?.status).toBe('working');
    expect(latest().find(n => n.id === 3)?.status).toBe('awaiting_input');

    act(() => {
      useAgentNodeStore.getState().patchAgentNode(2, { status: 'idle' });
    });

    expect(latest().find(n => n.id === 1)?.is_pinned).toBe(true);
    expect(latest().find(n => n.id === 2)?.status).toBe('idle');
    expect(latest().find(n => n.id === 3)?.status).toBe('awaiting_input');
    expect(latest().find(n => n.id === 3)?.is_pinned).toBe(true);
    // Mesh assignment is untouched by an unrelated status change.
    expect(latest().find(n => n.id === 2)?.mesh_id).toBe(2);
  });

  it('excludes archived rows only where the consumer filters them', () => {
    // `useAllAgentNodes` returns every live row including archived ones; the
    // archived filter lives in the consumers (Sidebar/grid). Memoization must
    // not silently start dropping them, which would break the Archive tab.
    seedAgentNodes([
      makeNode({ id: 1, status: 'idle' }),
      makeNode({ id: 2, status: 'archived' }),
    ]);
    render(createElement(Consumer));
    expect(latest().map(n => n.id)).toEqual([1, 2]);
  });
});