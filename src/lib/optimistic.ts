/**
 * Optimistic-with-rollback helper for `agentNodeStore` (issue #1054).
 *
 * Three call sites in `agentNodeStore.ts` —
 * `renameAgentNode`, `setNodePinned`, `toggleNodePinned` — used to
 * hand-roll the same shape: capture the prior row, patch the local state
 * immediately, await the mutation, then either adopt the backend's
 * returned `AgentNode` (pin actions) or let a separate event-driven
 * patch handle the rest (rename — `node-renamed` is emitted separately
 * and is also what keeps every other window in sync). On rejection they
 * all rolled back the patched columns and wrote `state.error`.
 *
 * This helper factors out the bookkeeping. It is **not** a replacement
 * for the component-local `optimisticToggle` (which targets a
 * React-component-owned `useState` triple of setters — see
 * `optimisticToggle.ts`). It is the Zustand-store equivalent.
 *
 * Scope of rollback — narrow, not full-row
 * -----------------------------------------
 * The rollback patch covers exactly the keys in `optimisticPatch`. Two
 * reasons:
 *
 *   1. **Concurrent writes survive.** A `node-renamed` event can fire
 *      while `setNodePinned` is in flight (e.g. the user renames a node,
 *      then immediately pins it). Restoring only `is_pinned` leaves the
 *      rename intact; restoring the whole pre-call snapshot would
 *      clobber it. The pin tests pin this invariant — see
 *      "rolls back ONLY is_pinned on rejection, preserving concurrent
 *      writes" in `tests/unit/agent-node-store.test.ts`.
 *   2. **`adoptResult` is full-row, rollback is narrow.** The backend's
 *      returned `AgentNode` is the source of truth on the happy path
 *      (a future refactor that mutates other columns on the way back
 *      would otherwise be invisible). On the failure path we can't
 *      trust the backend's response, so we restore only what we
 *      optimistically changed.
 *
 * Adoption is optional — `renameAgentNode` doesn't adopt, because a
 * separate `node-renamed` event is the cross-window source of truth
 * and adopting here would race against that listener.
 *
 * Not for component-local toggles — see `optimisticToggle.ts` for the
 * AppSettings pattern (component-owned setters, not a Zustand store).
 */
import { formatError } from './errorUtils';
import type { AgentNode } from '../types/generated/AgentNode';

/**
 * Narrow surface the helper writes through. Keeping the surface tight
 * (only `agentNodes` + `error`) means a future caller that holds any
 * Zustand store with that shape can reuse the helper without dragging
 * the rest of the agent-node state along.
 */
export interface OptimisticSurface {
  /** Read the current node list — only used to capture `prior` and to
   *  walk the list when patching. Cheap; the store is in-memory. */
  getAgentNodes: () => AgentNode[];
  /** Replace the agent-nodes array. Functional form only — matches
   *  Zustand's `set(updater)` and avoids stale-closure bugs. The
   *  helper passes a function; accepting an array would re-open a
   *  full-replace path the helper never needs (issue #1054 review). */
  setAgentNodes: (next: (prev: AgentNode[]) => AgentNode[]) => void;
  /** Write `state.error` on the rejection path. */
  setError: (error: string | null) => void;
}

export interface WithOptimisticArgs<TPatch extends Partial<AgentNode>, TResult> {
  surface: OptimisticSurface;
  /** Node id to update. The helper rejects with a clear error if the
   *  node isn't loaded — matches the explicit `prior` precondition in
   *  the three pre-refactor inline patterns. */
  nodeId: number;
  /** Columns to patch optimistically. Also the rollback scope — on
   *  rejection, exactly these keys are restored from the prior row.
   *  Passing `{ name }` rolls back `name` only, not the whole row. */
  optimisticPatch: TPatch;
  /** The backend mutation. The promise's resolved value is forwarded
   *  to `adoptResult`; its rejection drives the rollback path. */
  mutation: () => Promise<TResult>;
  /** Optional — adopt the mutation's resolved value as the new node
   *  (full-row replace). Used by the pin actions. Omit for
   *  `renameAgentNode`, where a separate `node-renamed` event is the
   *  source of truth. If `adoptResult` is set but returns
   *  `undefined` (e.g. the backend returned `void`), no adoption
   *  happens and the optimistic patch stands. */
  adoptResult?: (result: TResult) => AgentNode | undefined;
}

/**
 * Apply `optimisticPatch` to the local node immediately, run
 * `mutation`, then either adopt the returned `AgentNode` (if
 * `adoptResult` is set and returns one) or leave the optimistic
 * patch in place. On rejection: roll back the patched columns to
 * their pre-call values, write `state.error` via
 * `surface.setError(formatError(e))`, and re-throw.
 */
export async function withOptimistic<TPatch extends Partial<AgentNode>, TResult>(
  args: WithOptimisticArgs<TPatch, TResult>,
): Promise<TResult> {
  const prior = args.surface.getAgentNodes().find(n => n.id === args.nodeId);
  if (!prior) {
    throw new Error(`withOptimistic: node ${args.nodeId} is not loaded`);
  }
  // The rollback patch restores the prior values for each key the
  // optimistic patch touched — not the optimistic values themselves.
  // Same shape as `optimisticPatch`, built by reading `prior` per key.
  // Object.keys widens to `string`; the `as Partial<AgentNode>`
  // closure cast keeps the result type tight. A typed reducer would
  // require either a key-restricting generic constraint (which TS
  // rejects under strict variance — `keyof TPatch` doesn't satisfy
  // `Partial<AgentNode>`'s indexer) or a hand-rolled loop that adds
  // visual noise for a one-line rebuild.
  const rollbackPatch = Object.fromEntries(
    Object.keys(args.optimisticPatch).map(k => [k, (prior as unknown as Record<string, unknown>)[k]]),
  ) as Partial<AgentNode>;
  const applyOptimistic = (nodes: AgentNode[]) =>
    nodes.map(n => (n.id === args.nodeId ? { ...n, ...args.optimisticPatch } : n));
  const applyRollback = (nodes: AgentNode[]) =>
    nodes.map(n => (n.id === args.nodeId ? { ...n, ...rollbackPatch } : n));

  args.surface.setAgentNodes(applyOptimistic);

  try {
    const result = await args.mutation();
    if (args.adoptResult) {
      const adopted = args.adoptResult(result);
      if (adopted) {
        args.surface.setAgentNodes(nodes =>
          nodes.map(n => (n.id === args.nodeId ? adopted : n)),
        );
      }
    }
    return result;
  } catch (e) {
    args.surface.setAgentNodes(applyRollback);
    args.surface.setError(formatError(e));
    throw e;
  }
}