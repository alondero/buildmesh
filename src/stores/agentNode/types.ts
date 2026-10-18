import type { StoreApi } from 'zustand';
import type { AgentNode } from '../../types/generated/AgentNode';
import type { CircuitAgentOwnership } from '../../types/generated/CircuitAgentOwnership';
import type { SemanticTurnPayload } from '../../types/generated/SemanticTurnPayload';
import type { InputStall } from '../../lib/terminalInputQueue';

export interface SpawnAgentOptions {
  rows?: number;
  cols?: number;
  fresh?: boolean;
  /// First-turn prompt (issue #1413). Non-empty wins over `fresh` /
  /// resume and becomes `SpawnAgentIntent.loop`.
  prefill?: string;
}

export interface AgentNodeState {
  // Issue #1384 — normalized entity state. Two structures back the same data:
  //   `nodesById` — keyed lookup, preserves object identity per node so
  //                 per-id subscribers (`state.nodesById[id]`) skip renders
  //                 when only OTHER nodes change.
  //   `nodeIds`   — ordered list of ids (canonical `(mesh_id, position)`
  //                 order, matching `list_agent_nodes`); consumers that need
  //                 the ordered array iterate this and dereference through
  //                 `nodesById`. The id list itself is rebuilt on each
  //                 fetch, but the per-id object references are preserved
  //                 by the shallow reconciliation in `fetchAgentNodes`.
  nodesById: Record<number, AgentNode>;
  nodeIds: number[];
  // Circuit ownership comes from the run-step satellite ledger. Every Agent
  // Node spawned by one run resolves to the same run id shown in its header.
  circuitOwnerships: Record<number, CircuitAgentOwnership>;
  semanticTurns: Record<number, SemanticTurnPayload>;
  activeNodeId: number | null;
  loading: boolean;
  error: string | null;
  // Nodes whose close is in flight. The slow part of closing is the worktree
  // safety check (a git status + ref walk) that must run *before* we can drop
  // the row, so we flag the node here the instant the user clicks and let
  // NodeItem show a spinner instead of looking frozen.
  closingNodeIds: Set<number>;
  // Issue #1530 — sessions whose PTY input the transport's ordered retry
  // buffer is holding rather than delivering, keyed by node id. A *record*, not
  // a single slot: Buildmesh runs many agents in parallel, so two nodes can be
  // stalled at the same time, and with one shared slot the first node to recover
  // would withdraw a badge that another, still-wedged node still needs. Entries
  // exist only while a node is actually stalled, so the map is empty in the
  // normal case and the UI footprint is unchanged.
  stalledInputs: Record<number, InputStall>;

  // Derived getters
  /// Returns the ordered array of agent nodes (mesh_id, position) for code
  /// paths that genuinely need the list (test seeds, view-mode helpers,
  /// imperative `getState()` callers). Subscribed React components should
  /// NOT use this — they get O(N) re-renders on every fetch. Use
  /// `state.nodesById[id]` for per-node reads and a `useMemo` over
  /// `state.nodeIds.map(id => state.nodesById[id])` for full-list views.
  /// Each call returns a fresh array (cheap; no caching) — tests rely on
  /// this so `setState({ nodesById, nodeIds })` followed by a
  /// `getAgentNodes()` round-trip returns the freshly-seeded nodes.
  getAgentNodes: () => AgentNode[];
  getActiveNode: () => AgentNode | null;
  getActiveMeshId: () => number | null;

  fetchAgentNodes: () => Promise<void>;
  /// Conditional full refresh for additive events (issue #1751).
  /// Skips the `fetchAgentNodes` fan-out when the last successful
  /// snapshot is still fresh AND every scoped id is already in the map
  /// — the event then carries nothing the store doesn't have (e.g. a
  /// duplicate `node-created` for a row an earlier fetch already picked
  /// up). Fetches otherwise. Never use for evictions: an archived or
  /// deleted id is *supposed* to be in the map, and the fetch is what
  /// purges it — presence must not guard a removal (issue #1073
  /// pattern: a skipped fetch must never leave event state unapplied).
  refreshIfStale: (nodeIds?: number[]) => Promise<void>;
  createAgentNode: (meshId: number, name: string, path: string, branch: string, provider?: string, useWorktree?: boolean, configurationId?: string, initialPrompt?: string) => Promise<AgentNode>;
  /// Insert a node the backend just created but that did not come back through
  /// `createAgentNode` (e.g. `create_pr_node`'s reviewer sibling). Insert-if-
  /// absent: the `node-created` listener's async `fetchAgentNodes` may have
  /// already delivered the row, and its version must win. Exists so a caller
  /// can act on the new id immediately — `nodeActivityStore.groupNodes` bails
  /// unless both ids are in `nodesById`, so grouping a fresh spawn would
  /// otherwise race the event-driven refresh.
  adoptAgentNode: (node: AgentNode) => void;
  /// Sidebar "click + or pick provider" entrypoint — creates a node on the
  /// mesh, sets it active, and selects the mesh. The three steps live behind
  /// one action (issue #283) so the invariant — "only switch active mesh/node
  /// if creation succeeded" — is enforced in one place, not re-derivable per
  /// click handler. On `createAgentNode` rejection the active mesh stays put.
  /// `initialPrompt` (issue #1413) is an optional first-turn prompt.
  /// When non-empty, this action creates the node and then calls
  /// `spawnAgent` with `{ prefill }` in the same turn — Terminal
  /// auto-spawn is skipped because the node is already `spawning`.
  /// Omitted / whitespace means Fresh (Terminal auto-spawns as before).
  selectProviderForMesh: (meshId: number, meshName: string, meshPath: string, providerId: string, useWorktree?: boolean, initialPrompt?: string, configurationId?: string) => Promise<AgentNode>;
  deleteAgentNode: (id: number) => Promise<void>;
  /// Backend-driven retirement. The Circuit worker's `CloseAgentNode` effect
  /// and cancelled-run cleanup delete the row directly and emit `node-deleted`;
  /// this drops it from the grid and disposes its terminal. Unlike archive, a
  /// backend delete is a real deletion, so the terminal-persistence rule
  /// permits disposal. A no-op when the row is already absent — a
  /// user-initiated close removed it optimistically and disposes on its own
  /// success path.
  removeAgentNode: (id: number) => void;
  renameAgentNode: (id: number, name: string) => Promise<void>;
  /// Pin a node explicitly (wayfinder #982 / ticket #984). Used by the
  /// UI affordance when the user wants a known-good state (e.g. "Pin"
  /// in a context menu) — distinguishes from `toggleNodePinned`, which
  /// flips whatever the current value is. Optimistic: the local entry is
  /// patched from the returned `AgentNode` so the grid re-renders
  /// instantly; on rejection we revert *only* the `is_pinned` column to
  /// its pre-call value (not the whole entry) so concurrent writes to
  /// other columns survive, and surface the error on `state.error`.
  setNodePinned: (nodeId: number, pinned: boolean) => Promise<AgentNode>;
  /// Flip a node's `is_pinned` flag and patch the local entry from the
  /// returned `AgentNode` (wayfinder #982 / ticket #984). The
  /// single-action shape the UI's click-to-pin button uses. Same
  /// optimistic + narrow-`is_pinned`-rollback pattern as `setNodePinned`.
  toggleNodePinned: (nodeId: number) => Promise<AgentNode>;
  reorderAgentNode: (nodeId: number, insertIndex: number) => Promise<void>;
  swapAgentNodes: (aId: number, bId: number) => Promise<void>;
  /** Select the active agent entity. Activity-tab navigation is owned by
   * `useNodeActivityStore.activateNode`, which coordinates both stores at
   * the UI boundary without adding transient view state here.
   */
  setActiveNode: (id: number | null) => void;
  spawnAgent: (
    nodeId: number,
    provider: string,
    rowsOrOptions?: number | SpawnAgentOptions,
    cols?: number,
  ) => Promise<void>;
  /// Issue #774 — swap a node's Model Provider. The backend preserves
  /// worktree / branch / name / position; only `provider` changes. The
  /// returned `AgentNode` reflects the post-swap state (the backend has
  /// already updated the row), so the caller can patch the local entry
  /// without an extra `fetchAgentNodes` round-trip on the happy path.
  regenerateAgentNode: (nodeId: number, newProviderId: string) => Promise<AgentNode>;
  /// Issue #1306 — "Start Fresh" escape hatch. Discards the stale
  /// `cli_session_id` and boots a new session in the same existing worktree.
  /// Invokes `spawnAgent` with `options.fresh: true` (passing `resume: null`
  /// to the backend), causing `spawn_with_intent` to emit `SpawnIntent::Fresh`
  /// which clears `cli_session_id` in SQLite and spawns without `--resume`.
  restartFreshAgent: (
    nodeId: number,
    options?: { rows?: number; cols?: number },
  ) => Promise<void>;
  spawnHandoverAgent: (meshId: number, prefill: string, provider?: string) => Promise<AgentNode>;
  killAgent: (nodeId: number) => Promise<void>;
  sendToAgent: (nodeId: number, input: string) => Promise<void>;
  writeToAgent: (nodeId: number, data: string) => Promise<void>;
  /// Hand `text` over to an existing agent node's PTY as a submitted prompt
  /// (the terminal context menu's "Handover to node" action). The backend owns
  /// the bracketed-paste staging and the decoupled Enter — see
  /// [`api.handoverToAgent`].
  handoverToNode: (nodeId: number, text: string) => Promise<void>;
  clearAttention: (nodeId: number) => Promise<void>;
  // Issue #1054 — typed dispatch surface for `agentNodeListeners.ts`.
  // The listener module never reaches into `set`/`get` directly; it
  // dispatches through these actions (plus `fetchAgentNodes` /
  // `setActiveNode` from the public surface). Each action is a
  // one-liner that the store can also expose to other callers if a
  // future refactor needs the same seam.
  patchAgentNode: (id: number, patch: Partial<AgentNode>) => void;
  patchCircuitOwnershipState: (runId: number, state: string) => void;
  /// Re-read just the Circuit ownership ledger, without the full
  /// `fetchAgentNodes` fan-out. The implementation carries the reasoning.
  refreshCircuitOwnerships: () => Promise<void>;
  setSemanticTurn: (id: number, turn: SemanticTurnPayload | null) => void;
  /// Issue #1530 — publish (or clear) one session's stalled-input state, as
  /// reported by the transport's ordered retry buffer. Scoped by `nodeId` so
  /// parallel agents cannot withdraw each other's badges.
  setStalledInput: (nodeId: number, stall: InputStall | null) => void;
  findAgentNode: (id: number) => AgentNode | undefined;
  /// Issue #1054 — attach the store's Tauri event listeners; a one-line
  /// delegate to `agentNodeListeners.attachAgentNodeListeners`.
  /// Issue #1524 — the returned promise REJECTS if any listener fails to
  /// register, and the store returns to `idle` so a later call retries.
  /// The promise is shared by concurrent callers, so exactly one
  /// attachment is ever in flight.
  initAttentionListeners: () => Promise<void>;
  /// Issue #1530 — begin mirroring the transport's ordered retry buffer's
  /// stalled-input reports into [`stalledInput`]. Idempotent, and called by
  /// `initAttentionListeners` so the app's existing init path is enough.
  initTerminalInputWatch: () => void;
}

export type SetAgentNodeState = StoreApi<AgentNodeState>['setState'];
export type GetAgentNodeState = StoreApi<AgentNodeState>['getState'];

/** Per-store fences shared by refreshes, lifecycle events and domain actions. */
export interface AgentNodeRuntime {
  nodeRevision: number;
  spawnRequests: Map<number, symbol>;
  failedAutoSpawns: Set<number>;
}
