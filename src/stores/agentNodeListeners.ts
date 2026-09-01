/**
 * Event-listener module for `agentNodeStore` (issue #1054).
 *
 * Before this split, `initAttentionListeners` was a 130-line method on
 * `useAgentNodeStore` that inlined ten `listen<T>(...)` handlers. Each
 * handler reached into the store's private `set` and `get` directly —
 * the store's interface mixed actions, selectors, AND the event-driven
 * implementation. Three concerns in one place.
 *
 * After the split:
 *   - This module owns the event-name → action map (and the cache
 *     invalidation that two handlers also drive).
 *   - `agentNodeStore` owns the actions + state shape. Its
 *     `initAttentionListeners` becomes a one-line delegate.
 *   - Tests can swap in any object satisfying `AgentNodeActionSurface`
 *     to exercise dispatch in isolation — the surface is a typed seam
 *     with no transitive store imports.
 *
 * Why a typed surface, not bare `set/get`
 * ----------------------------------------
 * The listener only needs to *dispatch* — it never reads the store
 * shape (except for the cache-invalidation lookup, which uses a
 * dedicated `findAgentNode(id)` action). Passing the whole
 * `useAgentNodeStore` would re-couple the listeners to every store
 * change. The narrow surface keeps the listeners re-attachable when
 * the store evolves.
 *
 * Why a function call, not a message bus
 * ---------------------------------------
 * Issue #1054's third leg ("replace cross-store writes with typed
 * message-passing") was considered and deferred: every other cross-
 * store reach in the repo uses the same `.getState().action(...)` /
 * imperative-wrapper convention, and the test injection points already
 * in place (`vi.mock('toastStore')`, `setWorktreeCloseActionResolverForTests`)
 * provide the isolation a message bus would otherwise buy. Adding a
 * second indirection here would add a new abstraction layer for no
 * testability gain. The dispatch surface is the smallest contract that
 * keeps the listeners testable.
 */
import { listen } from '@tauri-apps/api/event';
import type { AttentionClearedPayload } from '../types/generated/AttentionClearedPayload';
import type { LifecycleChangedPayload } from '../types/generated/LifecycleChangedPayload';
import type { SemanticTurnPayload } from '../types/generated/SemanticTurnPayload';
import type { NodeRenamedPayload } from '../types/generated/NodeRenamedPayload';
import type { NodeCreatedPayload } from '../types/generated/NodeCreatedPayload';
import type { NodeActivatedPayload } from '../types/generated/NodeActivatedPayload';
import type { NodeSpawnCompletedPayload } from '../types/generated/NodeSpawnCompletedPayload';
import type { NodeSpawnFailedPayload } from '../types/generated/NodeSpawnFailedPayload';
import type { AutopilotFinishingPayload } from '../types/generated/AutopilotFinishingPayload';
import type { AutopilotPrCreatedPayload } from '../types/generated/AutopilotPrCreatedPayload';
import type { AutopilotFinishFailedPayload } from '../types/generated/AutopilotFinishFailedPayload';
import type { AutopilotNodeClosedPayload } from '../types/generated/AutopilotNodeClosedPayload';
import type { AgentNode } from '../types/generated/AgentNode';
import type { AutopilotRunState } from '../types/generated/AutopilotRunStateKind';
import { invalidateNodeCaches } from '../hooks/invalidateNodeCaches';
import { getNodeGitPath } from '../lib/paths';

/**
 * Typed dispatch seam for the agent-node event listeners. The store
 * implements every method; tests can substitute a fake. Each method
 * has a single responsibility so the listeners stay one-liners.
 */
export interface AgentNodeActionSurface {
  /** Refetch the full node list (used by `node-created` /
   *  `autopilot-node-closed`). The store's `fetchAgentNodes` is the
   *  canonical implementation; tests that want to assert the dispatch
 *   alone can pass a spy. */
  fetchAgentNodes: () => Promise<void>;
  /** Switch the active node synchronously (used by `node-activated`,
   *  the HTTP E2E test signal). */
  setActiveNode: (id: number | null) => void;
  /** Patch one or more columns of a single row. Used by every status
   *  transition (`awaiting_input`/`running`/`error`) and by
   *  `node-renamed`. */
  patchAgentNode: (id: number, patch: Partial<AgentNode>) => void;
  /** Patch the autopilot pill state for one node. Used by
   *  `autopilot-finishing` / `autopilot-pr-created` /
   *  `autopilot-finish-failed`. */
  patchAutopilotState: (id: number, state: AutopilotRunState) => void;
  /** Set or clear the structured action shown above an awaiting terminal. */
  setSemanticTurn: (id: number, turn: SemanticTurnPayload | null) => void;
  /** Read a single agent node by id — the cache-invalidation handlers
   *  need its git path. Returns `undefined` if the node is unknown
   *  (the caller treats that as a no-op; see `invalidateNodeCaches`
   *  for the structural-staleness reasoning). */
  findAgentNode: (id: number) => AgentNode | undefined;
}

// `attention-needed` / `attention-cleared` are the external
// AttentionHook protocol — the `session_id` payload key is the wire
// contract with already-deployed agent hooks (CONTEXT.md ambiguity #1
// says this stays as "session" intentionally). Map to the internal
// `node_id` alias for vocabulary consistency inside the store.
const SESSION_ID_KEY = 'session_id';

/**
 * Subscribe every agent-node Tauri event the store cares about.
 * Returns a single async-aggregated unlisten handle (issue #547-style
 * aggregation). The store calls this from `initAttentionListeners`
 * once, gated by a closure flag so React StrictMode's double-mount
 * doesn't double-register.
 *
 * Each handler is a one-liner that dispatches to a store action, so
 * adding a new event means (1) a generated payload type, (2) a small
 * `patchAgentNode` / `patchAutopilotState` line, (3) one entry here.
 */
export async function attachAgentNodeListeners(
  surface: AgentNodeActionSurface,
): Promise<() => void> {
  const unlistens: Array<() => void> = [];

  unlistens.push(
    await listen<AttentionClearedPayload>('attention-cleared', (event) => {
      const nodeId = event.payload[SESSION_ID_KEY];
      surface.setSemanticTurn(nodeId, null);
      surface.patchAgentNode(nodeId, { status: 'running' });
    }),
  );

  // `agent-lifecycle` (issue #1364) is the normalized lifecycle event —
  // the wire carries the resulting status, the normalized kind, and the
  // full provider envelope. Both transports (this desktop bus and the
  // mobile /ws/events broadcast) share the one shape, so both clients
  // patch the affected node identically. The backend emits this event on
  // EVERY mark transition (alongside the legacy desktop `attention-needed`
  // event, which external hook consumers may still observe) — so this
  // handler is the ONLY store-side listener for marks; there is no
  // attention-needed store listener to duplicate its state mutations.
  // The status + health patches are batched into one `patchAgentNode` so
  // the store updates once per event.
  unlistens.push(
    await listen<LifecycleChangedPayload>('agent-lifecycle', (event) => {
      const nodeId = event.payload.session_id;
      const { signal_health } = event.payload;
      surface.patchAgentNode(nodeId, {
        status: event.payload.status,
        ...(signal_health ? { signal_health } : {}),
      });
      if (event.payload.semantic_turn) {
        surface.setSemanticTurn(nodeId, event.payload.semantic_turn);
      } else if (
        event.payload.kind === 'turn_completed' ||
        event.payload.kind === 'autopilot_completed' ||
        event.payload.kind === 'input_required' ||
        event.payload.kind === 'permission_requested' ||
        event.payload.kind === 'question_requested'
      ) {
        surface.setSemanticTurn(nodeId, null);
      }
    }),
  );

  // `node-renamed` is the internal event emitted by `rename_agent_node`
  // (renamed from `session-renamed` in issue #490). The payload key
  // follows: `node_id`, not `session_id`.
  unlistens.push(
    await listen<NodeRenamedPayload>('node-renamed', (event) => {
      const { node_id: nodeId, name } = event.payload;
      surface.patchAgentNode(nodeId, { name });
    }),
  );

  // `node-created` is fired by the two-stage desktop spawn flow (after
  // creating the `pending` node, so the sidebar picks up the new node
  // before stage-2 finishes) and the HTTP-based E2E test server.
  // A refetch is race-free because the row is already committed by
  // the time the event fires.
  unlistens.push(
    await listen<NodeCreatedPayload>('node-created', async () => {
      await surface.fetchAgentNodes();
    }),
  );

  // `node-activated` is fired by the HTTP-based E2E test server.
  unlistens.push(
    await listen<NodeActivatedPayload>('node-activated', (event) => {
      surface.setActiveNode(event.payload.node_id);
    }),
  );

  // Two-stage spawn completion: backend reports the agent process is
  // up. The header chips' cached `null` for a node is structurally
  // wrong at this moment (the worktree now exists), so we invalidate
  // — issue #1004. No-op for an unseen node.
  unlistens.push(
    await listen<NodeSpawnCompletedPayload>('node-spawn-completed', (event) => {
      const nodeId = event.payload.node_id;
      surface.patchAgentNode(nodeId, { status: 'running' });
      const node = surface.findAgentNode(nodeId);
      if (node) {
        invalidateNodeCaches(nodeId, getNodeGitPath(node));
      }
    }),
  );

  // Autopilot pipeline transitions: patch the pill state in place so
  // the header tracks the run without waiting for the next full
  // refetch. App.tsx separately refetches on the completion/failure
  // events to pick up the node's own status change.
  unlistens.push(
    await listen<AutopilotFinishingPayload>('autopilot-finishing', (event) => {
      surface.patchAutopilotState(event.payload.node_id, 'finishing');
    }),
  );
  unlistens.push(
    await listen<AutopilotPrCreatedPayload>('autopilot-pr-created', (event) => {
      surface.patchAutopilotState(event.payload.node_id, 'completed');
      // The wrap-up just opened a PR, so the chip's cached "no PR" is
      // wrong — and up to 60s of freshness window stands between it
      // and the next bus-driven refetch. Issue #1004.
      const node = surface.findAgentNode(event.payload.node_id);
      if (node) {
        invalidateNodeCaches(event.payload.node_id, getNodeGitPath(node));
      }
    }),
  );
  unlistens.push(
    await listen<AutopilotFinishFailedPayload>('autopilot-finish-failed', (event) => {
      surface.patchAutopilotState(event.payload.node_id, 'failed');
    }),
  );

  // Merged-PR auto-close: the backend archived the node (NOT deleted);
  // refetch so the card leaves the grid. We deliberately do NOT
  // dispose the terminal — archive keeps the row, branch, and
  // scrollback alive for the Archive tab, and the terminal-persistence
  // rule says only a node-delete may dispose. `TerminalManager` is a
  // singleton; the instance survives the refetch.
  unlistens.push(
    await listen<AutopilotNodeClosedPayload>('autopilot-node-closed', async () => {
      await surface.fetchAgentNodes();
    }),
  );

  // Two-stage spawn failure: backend already updated the node's DB
  // status to 'error' before emitting — we mirror it in the store so
  // the sidebar/title-bar renders the red badge without a refetch.
  unlistens.push(
    await listen<NodeSpawnFailedPayload>('node-spawn-failed', (event) => {
      surface.patchAgentNode(event.payload.node_id, { status: 'error' });
    }),
  );

  return () => {
    for (const fn of unlistens) fn();
  };
}
