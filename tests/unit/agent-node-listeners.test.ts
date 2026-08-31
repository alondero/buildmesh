import { describe, it, expect, beforeEach, afterEach, vi } from 'vitest';
import { listen } from '@tauri-apps/api/event';
import {
  attachAgentNodeListeners,
  type AgentNodeActionSurface,
} from '../../src/stores/agentNodeListeners';
import type { AgentNode } from '../../src/types/generated/AgentNode';

/**
 * Unit tests for `attachAgentNodeListeners` (issue #1054).
 *
 * The store's `initAttentionListeners` now delegates to this module.
 * The listener module owns the event-name → action map; the store
 * provides a typed `AgentNodeActionSurface`. These tests substitute a
 * spy surface and assert each event fires the right dispatch — no
 * Zustand store, no IPC, no terminal mocks required.
 *
 * The end-to-end coverage (store wires the surface correctly + the
 * mockEmit helpers fire real handlers) lives in
 * `tests/unit/agent-node-store.test.ts` and
 * `tests/unit/node-cache-invalidation.test.ts`. Here the listeners
 * are exercised in isolation.
 */

interface SpySurface extends AgentNodeActionSurface {
  __calls: { method: string; args: unknown[] }[];
}

function makeSurface(nodes: AgentNode[] = []): SpySurface {
  // One recorder per surface method: pushes onto __calls on every
  // invocation. The `as unknown as T` cast is needed because `vi.fn`
  // can't preserve the original return-type narrower than its
  // implementation signature here — kept local to the helper so the
  // per-test assertions stay terse.
  const __calls: { method: string; args: unknown[] }[] = [];
  const spy = <T extends (...a: unknown[]) => unknown>(
    method: string,
    impl: T,
  ): SpySurface[typeof method] =>
    vi.fn((...args: unknown[]) => {
      __calls.push({ method, args });
      return impl(...args);
    }) as unknown as SpySurface[typeof method];
  return {
    fetchAgentNodes: spy('fetchAgentNodes', async () => undefined),
    setActiveNode: spy('setActiveNode', () => {}),
    patchAgentNode: spy('patchAgentNode', () => {}),
    patchAutopilotState: spy('patchAutopilotState', () => {}),
    setSemanticTurn: spy('setSemanticTurn', () => {}),
    findAgentNode: spy('findAgentNode', (id: number) =>
      nodes.find(n => n.id === id),
    ),
    __calls,
  };
}

describe('attachAgentNodeListeners', () => {
  beforeEach(() => {
    vi.clearAllMocks();
  });

  // Each test below overrides `listen`'s mock implementation to
  // capture the handler it registers. `vi.clearAllMocks()` clears
  // calls/results but does NOT restore the default implementation —
  // we need `mockReset()` so the next test sees the setup's default
  // (which adds to the `mockListeners` Map and is what every other
  // test file relies on). Without this, the second test in this
  // file would inherit the previous test's handler-capture shim.
  afterEach(() => {
    vi.mocked(listen).mockReset();
  });

  it('registers a handler for every agent-node event the store cares about', async () => {
    const surface = makeSurface();

    await attachAgentNodeListeners(surface);

    // Ten event subscriptions should be live after attach — one per
    // event the store cares about. We read the setup's listener map
    // directly rather than going through `listen`'s mock, because
    // the mock returns Promise<unlistenFn> per call and doesn't
    // expose the underlying map.
    const mockListen = listen as ReturnType<typeof vi.fn>;
    const eventNames = mockListen.mock.calls.map(([name]) => name);
    expect(eventNames).toEqual(expect.arrayContaining([
      'attention-needed',
      'attention-cleared',
      'agent-lifecycle',
      'node-renamed',
      'node-created',
      'node-activated',
      'node-spawn-completed',
      'node-spawn-failed',
      'autopilot-finishing',
      'autopilot-pr-created',
      'autopilot-finish-failed',
      'autopilot-node-closed',
    ]));
    expect(eventNames).toHaveLength(12);
  });

  it('returns a single unlisten handle that detaches every registered handler', async () => {
    // The setup's `listen` mock returns a Promise<() => void>. We
    // capture every unlistenFn and assert attach returns a function
    // that calls each one — issue #547-style aggregation.
    const unlistenFns: Array<() => void> = [];
    const mockListen = listen as ReturnType<typeof vi.fn>;
    mockListen.mockImplementation(() => {
      const fn = () => unlistenFns.push(fn);
      return Promise.resolve(fn);
    });

    const surface = makeSurface();
    const unlisten = await attachAgentNodeListeners(surface);

    expect(typeof unlisten).toBe('function');
    // 12 events → 12 unlisten registrations.
    expect(mockListen).toHaveBeenCalledTimes(12);
    expect(unlistenFns).toHaveLength(0);

    unlisten();
    expect(unlistenFns).toHaveLength(12);
  });

  // The narrow surface contract: every handler must dispatch to the
  // store via the surface, not via some other path. We test this by
  // calling the handler we registered (the second arg to listen) and
  // asserting only the expected surface methods were called.
  it('attention-needed dispatches patchAgentNode with status awaiting_input', async () => {
    const mockListen = listen as ReturnType<typeof vi.fn>;
    let capturedHandler: ((event: { payload: unknown }) => void) | undefined;
    mockListen.mockImplementation((eventName: string, handler: (event: { payload: unknown }) => void) => {
      if (eventName === 'attention-needed') {
        capturedHandler = handler;
      }
      return Promise.resolve(() => {});
    });

    const surface = makeSurface();
    await attachAgentNodeListeners(surface);

    expect(capturedHandler).toBeDefined();
    capturedHandler!({ payload: { session_id: 42, semantic_turn: null } });

    expect(surface.__calls).toEqual([
      { method: 'setSemanticTurn', args: [42, null] },
      { method: 'patchAgentNode', args: [42, { status: 'awaiting_input' }] },
    ]);
  });

  it('attention-cleared dispatches patchAgentNode with status running', async () => {
    const mockListen = listen as ReturnType<typeof vi.fn>;
    let capturedHandler: ((event: { payload: unknown }) => void) | undefined;
    mockListen.mockImplementation((eventName: string, handler: (event: { payload: unknown }) => void) => {
      if (eventName === 'attention-cleared') {
        capturedHandler = handler;
      }
      return Promise.resolve(() => {});
    });

    const surface = makeSurface();
    await attachAgentNodeListeners(surface);

    capturedHandler!({ payload: { session_id: 7 } });

    expect(surface.__calls).toEqual([
      { method: 'setSemanticTurn', args: [7, null] },
      { method: 'patchAgentNode', args: [7, { status: 'running' }] },
    ]);
  });

  it('agent-lifecycle dispatches the resulting status + signal health (issue #1364)', async () => {
    const mockListen = listen as ReturnType<typeof vi.fn>;
    let capturedHandler: ((event: { payload: unknown }) => void) | undefined;
    mockListen.mockImplementation((eventName: string, handler: (event: { payload: unknown }) => void) => {
      if (eventName === 'agent-lifecycle') {
        capturedHandler = handler;
      }
      return Promise.resolve(() => {});
    });

    const surface = makeSurface();
    await attachAgentNodeListeners(surface);

    capturedHandler!({
      payload: {
        session_id: 42,
        kind: 'turn_completed',
        status: 'ready',
        message: 'turn finished',
        provider_event: 'Stop',
        provider_session_id: null,
        completion_reason: 'end_turn',
        transcript_path: null,
        timestamp: '2026-08-31T00:00:00+00:00',
        signal_health: 'ok',
        semantic_turn: null,
      },
    });

    expect(surface.__calls).toEqual([
      { method: 'patchAgentNode', args: [42, { status: 'ready' }] },
      { method: 'patchAgentNode', args: [42, { signal_health: 'ok' }] },
      { method: 'setSemanticTurn', args: [42, null] },
    ]);
  });

  it('agent-lifecycle with a semantic turn patches it through (issue #1364)', async () => {
    const mockListen = listen as ReturnType<typeof vi.fn>;
    let capturedHandler: ((event: { payload: unknown }) => void) | undefined;
    mockListen.mockImplementation((eventName: string, handler: (event: { payload: unknown }) => void) => {
      if (eventName === 'agent-lifecycle') {
        capturedHandler = handler;
      }
      return Promise.resolve(() => {});
    });

    const surface = makeSurface();
    await attachAgentNodeListeners(surface);

    capturedHandler!({
      payload: {
        session_id: 42,
        kind: 'permission_requested',
        status: 'awaiting_input',
        message: null,
        provider_event: 'PermissionRequest',
        provider_session_id: null,
        completion_reason: null,
        transcript_path: null,
        timestamp: '2026-08-31T00:00:00+00:00',
        signal_health: 'ok',
        semantic_turn: {
          node_id: 42,
          kind: 'permission_request',
          description: 'Allow edit: src/lib/auth.ts',
        },
      },
    });

    const calls = surface.__calls;
    expect(calls[0]).toEqual({ method: 'patchAgentNode', args: [42, { status: 'awaiting_input' }] });
    expect(calls[1]).toEqual({ method: 'patchAgentNode', args: [42, { signal_health: 'ok' }] });
    // The semantic turn flows to the banner; it is NOT cleared.
    expect(calls[2]).toEqual({
      method: 'setSemanticTurn',
      args: [42, { node_id: 42, kind: 'permission_request', description: 'Allow edit: src/lib/auth.ts' }],
    });
  });

  it('node-renamed dispatches patchAgentNode with the new name', async () => {
    const mockListen = listen as ReturnType<typeof vi.fn>;
    let capturedHandler: ((event: { payload: unknown }) => void) | undefined;
    mockListen.mockImplementation((eventName: string, handler: (event: { payload: unknown }) => void) => {
      if (eventName === 'node-renamed') {
        capturedHandler = handler;
      }
      return Promise.resolve(() => {});
    });

    const surface = makeSurface();
    await attachAgentNodeListeners(surface);

    capturedHandler!({ payload: { node_id: 11, name: 'fix-auth-flow' } });

    expect(surface.__calls).toEqual([
      { method: 'patchAgentNode', args: [11, { name: 'fix-auth-flow' }] },
    ]);
  });

  it('node-activated dispatches setActiveNode', async () => {
    const mockListen = listen as ReturnType<typeof vi.fn>;
    let capturedHandler: ((event: { payload: unknown }) => void) | undefined;
    mockListen.mockImplementation((eventName: string, handler: (event: { payload: unknown }) => void) => {
      if (eventName === 'node-activated') {
        capturedHandler = handler;
      }
      return Promise.resolve(() => {});
    });

    const surface = makeSurface();
    await attachAgentNodeListeners(surface);

    capturedHandler!({ payload: { node_id: 99 } });

    expect(surface.__calls).toEqual([
      { method: 'setActiveNode', args: [99] },
    ]);
  });

  it('node-spawn-completed dispatches patchAgentNode + invalidateNodeCaches (issue #1004)', async () => {
    const mockListen = listen as ReturnType<typeof vi.fn>;
    let capturedHandler: ((event: { payload: unknown }) => void) | undefined;
    mockListen.mockImplementation((eventName: string, handler: (event: { payload: unknown }) => void) => {
      if (eventName === 'node-spawn-completed') {
        capturedHandler = handler;
      }
      return Promise.resolve(() => {});
    });

    const surface = makeSurface([
      { id: 50, mesh_id: 1, name: 'n50', path: '/p/50', branch: 'main', env: 'windows', provider: 'anthropic', status: 'pending', created_at: '', use_worktree: true, position: 0, is_pinned: false },
    ]);
    await attachAgentNodeListeners(surface);

    capturedHandler!({ payload: { node_id: 50 } });

    // Two surface calls: patch status + find the row for cache
    // invalidation. The `findAgentNode` happens unconditionally; the
    // invalidation itself is internal to the listener module.
    expect(surface.__calls.map(c => c.method)).toEqual([
      'patchAgentNode',
      'findAgentNode',
    ]);
    expect(surface.__calls[0].args).toEqual([50, { status: 'running' }]);
    expect(surface.__calls[1].args).toEqual([50]);
  });

  it('node-spawn-completed no-ops the cache invalidation for an unseen node', async () => {
    const mockListen = listen as ReturnType<typeof vi.fn>;
    let capturedHandler: ((event: { payload: unknown }) => void) | undefined;
    mockListen.mockImplementation((eventName: string, handler: (event: { payload: unknown }) => void) => {
      if (eventName === 'node-spawn-completed') {
        capturedHandler = handler;
      }
      return Promise.resolve(() => {});
    });

    // The store has no row for node 999 — `findAgentNode` returns
    // undefined and the listener skips cache invalidation.
    const surface = makeSurface([]);
    await attachAgentNodeListeners(surface);

    capturedHandler!({ payload: { node_id: 999 } });

    expect(surface.__calls.map(c => c.method)).toEqual([
      'patchAgentNode',
      'findAgentNode',
    ]);
  });

  it('autopilot-finishing dispatches patchAutopilotState', async () => {
    const mockListen = listen as ReturnType<typeof vi.fn>;
    let capturedHandler: ((event: { payload: unknown }) => void) | undefined;
    mockListen.mockImplementation((eventName: string, handler: (event: { payload: unknown }) => void) => {
      if (eventName === 'autopilot-finishing') {
        capturedHandler = handler;
      }
      return Promise.resolve(() => {});
    });

    const surface = makeSurface();
    await attachAgentNodeListeners(surface);

    capturedHandler!({ payload: { node_id: 17 } });

    expect(surface.__calls).toEqual([
      { method: 'patchAutopilotState', args: [17, 'finishing'] },
    ]);
  });

  it('autopilot-pr-created dispatches patchAutopilotState + findAgentNode (cache invalidation)', async () => {
    const mockListen = listen as ReturnType<typeof vi.fn>;
    let capturedHandler: ((event: { payload: unknown }) => void) | undefined;
    mockListen.mockImplementation((eventName: string, handler: (event: { payload: unknown }) => void) => {
      if (eventName === 'autopilot-pr-created') {
        capturedHandler = handler;
      }
      return Promise.resolve(() => {});
    });

    const surface = makeSurface([
      { id: 17, mesh_id: 1, name: 'n17', path: '/p/17', branch: 'main', env: 'windows', provider: 'anthropic', status: 'running', created_at: '', use_worktree: true, position: 0, is_pinned: false },
    ]);
    await attachAgentNodeListeners(surface);

    capturedHandler!({ payload: { node_id: 17, pr_url: 'https://example/pr/1' } });

    expect(surface.__calls.map(c => c.method)).toEqual([
      'patchAutopilotState',
      'findAgentNode',
    ]);
    expect(surface.__calls[0].args).toEqual([17, 'completed']);
  });

  it('autopilot-finish-failed dispatches patchAutopilotState with failed', async () => {
    const mockListen = listen as ReturnType<typeof vi.fn>;
    let capturedHandler: ((event: { payload: unknown }) => void) | undefined;
    mockListen.mockImplementation((eventName: string, handler: (event: { payload: unknown }) => void) => {
      if (eventName === 'autopilot-finish-failed') {
        capturedHandler = handler;
      }
      return Promise.resolve(() => {});
    });

    const surface = makeSurface();
    await attachAgentNodeListeners(surface);

    capturedHandler!({ payload: { node_id: 23 } });

    expect(surface.__calls).toEqual([
      { method: 'patchAutopilotState', args: [23, 'failed'] },
    ]);
  });

  it('node-created triggers fetchAgentNodes (refetch, not append)', async () => {
    const mockListen = listen as ReturnType<typeof vi.fn>;
    let capturedHandler: ((event: { payload: unknown }) => void) | undefined;
    mockListen.mockImplementation((eventName: string, handler: (event: { payload: unknown }) => void) => {
      if (eventName === 'node-created') {
        capturedHandler = handler;
      }
      return Promise.resolve(() => {});
    });

    const surface = makeSurface();
    await attachAgentNodeListeners(surface);

    await capturedHandler!({ payload: { id: 99 } });

    expect(surface.__calls.map(c => c.method)).toEqual(['fetchAgentNodes']);
    expect(surface.fetchAgentNodes).toHaveBeenCalledTimes(1);
  });

  it('autopilot-node-closed triggers fetchAgentNodes (no dispose)', async () => {
    const mockListen = listen as ReturnType<typeof vi.fn>;
    let capturedHandler: ((event: { payload: unknown }) => void) | undefined;
    mockListen.mockImplementation((eventName: string, handler: (event: { payload: unknown }) => void) => {
      if (eventName === 'autopilot-node-closed') {
        capturedHandler = handler;
      }
      return Promise.resolve(() => {});
    });

    const surface = makeSurface();
    await attachAgentNodeListeners(surface);

    await capturedHandler!({ payload: { node_id: 7 } });

    // archive keeps the row/branch/scrollback; only refetch. The
    // terminal-persistence rule says only delete disposes.
    expect(surface.__calls.map(c => c.method)).toEqual(['fetchAgentNodes']);
  });

  it('node-spawn-failed dispatches patchAgentNode with status error', async () => {
    const mockListen = listen as ReturnType<typeof vi.fn>;
    let capturedHandler: ((event: { payload: unknown }) => void) | undefined;
    mockListen.mockImplementation((eventName: string, handler: (event: { payload: unknown }) => void) => {
      if (eventName === 'node-spawn-failed') {
        capturedHandler = handler;
      }
      return Promise.resolve(() => {});
    });

    const surface = makeSurface();
    await attachAgentNodeListeners(surface);

    capturedHandler!({ payload: { node_id: 33 } });

    expect(surface.__calls).toEqual([
      { method: 'patchAgentNode', args: [33, { status: 'error' }] },
    ]);
  });
});
