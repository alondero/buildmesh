/**
 * `agentNodeStore.initAttentionListeners` attachment contract (issue #1524).
 *
 * The listener module has its own rollback tests (see
 * `agent-node-listeners.test.ts`); this file pins the *store* half: a
 * failed registration must be repairable by Retry instead of leaving the
 * store permanently deaf to lifecycle events, and the repaired
 * attachment must be exactly one subscription per event.
 *
 * The attachment state lives in a `create()` closure, so it is one
 * instance per test file. `resetAgentNodeListenersForTests` returns it to
 * `idle` in `beforeEach`, which is what keeps these tests independent: an
 * earlier test that attached must not become this one's precondition, and
 * either test must pass when run alone (`-t <name>`).
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { listen } from '@tauri-apps/api/event';

// `agentNodeStore` statically imports `disposeTerminal` for the delete
// path (retained for #1252). Mock the module so this file never pulls the
// xterm surface in — the same shape as the existing store test.
vi.mock('../../src/components/Terminal/Terminal', () => ({
  disposeTerminal: vi.fn(),
  AgentTerminal: () => null,
}));

import { useAgentNodeStore, resetAgentNodeListenersForTests } from '../../src/stores/agentNodeStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import type { AgentNode } from '../../src/stores/agentNodeStore';

type EventHandler = (event: { payload: never }) => void;

interface ListenHarness {
  /** Make the Nth registration (1-based, counting `listen` calls) reject. */
  failRegistration: (ordinal: number | null) => void;
  /** Names of every event with at least one live handler. */
  liveEvents: () => string[];
  /** Live handler count for one event. */
  handlerCount: (event: string) => number;
  /** Total live handlers across all events. */
  totalHandlers: () => number;
  /** How many times `listen` was called (including rolled-back attempts). */
  totalRegistrations: () => number;
  /** Event names whose unlisten callback ran, in order. */
  unlistenLog: () => string[];
  /** Deliver one event to every live handler for it. */
  fire: (event: string, payload: unknown) => void;
}

/**
 * Replaces `listen` with a harness that tracks LIVE handlers per event, so
 * a rolled-back registration is observable as "no handler for that event"
 * rather than as a call that happened to resolve.
 */
function installListenHarness(): ListenHarness {
  const live = new Map<string, Set<EventHandler>>();
  const unlistenLog: string[] = [];
  let calls = 0;
  let failOrdinal: number | null = null;

  (listen as ReturnType<typeof vi.fn>).mockImplementation(
    (event: string, handler: EventHandler) => {
      calls += 1;
      if (failOrdinal === calls) {
        return Promise.reject(new Error(`listen failed: ${event}`));
      }
      const handlers = live.get(event) ?? new Set<EventHandler>();
      handlers.add(handler);
      live.set(event, handlers);
      return Promise.resolve(() => {
        handlers.delete(handler);
        unlistenLog.push(event);
      });
    },
  );

  return {
    failRegistration: (ordinal) => { failOrdinal = ordinal; },
    liveEvents: () => [...live.keys()],
    handlerCount: (event) => live.get(event)?.size ?? 0,
    totalHandlers: () =>
      [...live.values()].reduce((total, set) => total + set.size, 0),
    totalRegistrations: () => calls,
    unlistenLog: () => [...unlistenLog],
    fire: (event, payload) => {
      live.get(event)?.forEach((handler) => handler({ payload } as never));
    },
  };
}

function makeNode(overrides: Partial<AgentNode> = {}): AgentNode {
  return {
    id: 7, mesh_id: 1, name: 'keen-brook', path: '/a', branch: 'main',
    env: 'windows', provider: 'anthropic', status: 'running', created_at: '',
    use_worktree: true, position: 0,
    ...overrides,
  };
}

describe('initAttentionListeners (issue #1524: retryable attachment)', () => {
  // The attachment state is per test file, so every test starts from
  // `idle` regardless of what ran before it (or of running alone).
  beforeEach(() => {
    resetAgentNodeListenersForTests();
  });

  it('rolls a failed attachment back, repairs it on retry, and dispatches one update per event', async () => {
    const harness = installListenHarness();
    const init = () => useAgentNodeStore.getState().initAttentionListeners();

    // The third registration is `attention-cleared`.
    harness.failRegistration(3);
    await expect(init()).rejects.toThrow('listen failed: attention-cleared');

    // The two handlers that registered before the failure were removed,
    // and nothing after it registered — the bus is clean, not half-wired.
    expect(harness.unlistenLog()).toEqual([
      'circuit-run-updated',
      'circuit-pr-ready',
    ]);
    expect(harness.totalHandlers()).toBe(0);

    // Retry (the Boot Error Panel's Retry) with a healthy event bus. Two
    // callers race here because React StrictMode double-mounts App's
    // init effect; they must share ONE attachment.
    harness.failRegistration(null);
    await Promise.all([init(), init()]);

    // Exactly one handler per event — no event is subscribed twice.
    expect(harness.liveEvents()).toHaveLength(10);
    for (const event of harness.liveEvents()) {
      expect(harness.handlerCount(event)).toBe(1);
    }

    // A lifecycle event after the retry causes exactly one store update.
    seedAgentNodes([makeNode()]);
    let updates = 0;
    const unsubscribe = useAgentNodeStore.subscribe(() => { updates += 1; });
    // `process_running` is the patch-only lifecycle kind (the handler
    // returns before `setSemanticTurn`), so one dispatch = one update.
    harness.fire('agent-lifecycle', {
      session_id: 7,
      provider: 'anthropic',
      kind: 'process_running',
      status: 'awaiting_input',
      message: null,
      provider_event: null,
      provider_session_id: null,
      completion_reason: null,
      timestamp: '2026-10-04T00:00:00Z',
      signal_health: null,
      semantic_turn: null,
    });
    unsubscribe();

    expect(updates).toBe(1);
    expect(useAgentNodeStore.getState().nodesById[7].status).toBe('awaiting_input');
  });

  it('does not re-register once attached', async () => {
    const harness = installListenHarness();
    const init = () => useAgentNodeStore.getState().initAttentionListeners();

    // First call attaches the full set.
    await init();
    const afterFirstCall = harness.totalRegistrations();
    expect(afterFirstCall).toBe(10);

    // The second call short-circuits: it never reaches the event bus, so
    // no event ends up subscribed twice.
    await init();

    expect(harness.totalRegistrations()).toBe(afterFirstCall);
    expect(harness.totalHandlers()).toBe(10);
  });
});
