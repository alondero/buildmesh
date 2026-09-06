import { formatError } from '../lib/errorUtils';
import { create } from 'zustand';
import { useShallow } from 'zustand/react/shallow';
import * as api from '../lib/tauri';
import { disposeTerminal } from '../components/Terminal/Terminal'; // retained for delete path; archive must NOT dispose — see CLAUDE.md terminal-persistence rule.
import { hasWorktreeCloseRisk, type WorktreeCloseAction, type WorktreeCloseSafety } from '../lib/worktreeClose';
import { requestWorktreeCloseAction } from './worktreeClosePromptStore';
// Issue #1001 — `deleteAgentNode` Phase 2 (delete_commit) now surfaces
// its failure via the shared toast pipeline instead of the previously
// silent `state.error` Zustand field. Phase 1 (worktree safety check)
// is unchanged — it still sets `state.error`, which App.tsx routes
// through the generic "System" toast pipeline.
import { addToast } from './toastStore';
import { useMeshStore } from './meshStore';
// Issue #1054 — the event-driven half of the store moved to a typed
// listeners module (`agentNodeListeners.ts`); the optimistic-with-
// rollback pattern that `renameAgentNode` / `setNodePinned` /
// `toggleNodePinned` hand-rolled is now a generic helper.
import { withOptimistic, type OptimisticSurface } from '../lib/optimistic';
import { attachAgentNodeListeners } from './agentNodeListeners';
import { activityRootId } from '../lib/nodeActivities';
import { useNodeActivityStore } from './nodeActivityStore';

// `AgentNode` is generated from the Rust `models::AgentNode` struct (issue
// #359), along with the `EnvType`/`Provider`/`SessionStatus` unions it
// references. Imported for local use and re-exported so existing
// `import { AgentNode } from '../stores/agentNodeStore'` call sites keep
// working. The generated type is the full wire shape: nullable
// `cli_session_id`/`worktree_name` are `string | null` (not `?: string`), and
// it adds `env`, `source_issue`, and the `archived` status the hand-written
// interface omitted.
import type { AgentNode } from '../types/generated/AgentNode';
import type { AutopilotRunState } from '../types/generated/AutopilotRunStateKind';
import type { CircuitAgentOwnership } from '../types/generated/CircuitAgentOwnership';
import type { SemanticTurnPayload } from '../types/generated/SemanticTurnPayload';
import type { SpawnAgentIntent } from '../types/generated/SpawnAgentIntent';
export type { AgentNode };

// Issue #1054 — cross-store reach, kept narrow on purpose
// --------------------------------------------------------
// The proposal's third leg ("replace cross-store writes with typed
// message-passing") was considered and deferred: every other cross-
// store reach in the repo follows the same `.getState().action(...)`
// / imperative-wrapper convention, and the test injection points
// already in place (vi.mock for addToast; setWorktreeCloseActionResolverForTests;
// useMeshStore.setState in tests) provide the isolation a message bus
// would otherwise buy. Adding a second indirection here would add a
// new abstraction layer for no testability gain.
//
// The remaining four reach-throughs (one site each):
//   * useMeshStore.getState().selectMesh   — selectProviderForMesh (issue #283 invariant)
//   * requestWorktreeCloseAction            — deleteAgentNode Phase 1 safety prompt
//   * addToast                              — deleteAgentNode Phase 2 failure (issue #1001)
//   * disposeTerminal                       — deleteAgentNode success path (issue #647)
// Any future refactor that needs to replace one of these with a bus
// call can do so in-place; the listener module already shows the
// "typed dispatch surface" pattern that would scale.

// Issue #1384 — shallow-equal reconciliation. When `fetchAgentNodes` returns
// a fresh list, we compare each row against the existing `nodesById[id]`
// field-by-field. If every field matches, we keep the *old object reference*
// so subscribed components (`state.nodesById[id]` or `state.agentNodes.find(...)`)
// don't see a new ref and Zustand's `Object.is` selector skips the render.
// Fields not present on either side (e.g. an `undefined` row missing a
// generated `null`) are treated as equal so a wire-shape tweak on the Rust
// side doesn't churn every component on every fetch.
//
// Why the custom loop and not `Object.is`-per-field via JSON.stringify —
// `JSON.stringify` allocates a string per node and is order-sensitive for
// key insertion (V8 object-key order is insertion-order for string keys, so
// the only false-positive is if the Rust backend ever reorders its columns;
// in that case we'd silently fail to reconcile and the cascade comes back).
// A typed field-by-field compare is fast (≤20 primitives per node in the
// current wire shape), deterministic, and pins the contract in the type
// system via the exhaustiveness trick below.
// Exhaustiveness contract — the comparator MUST list every field on
// `AgentNode` except `id`. If the Rust struct gains a field, `tsc` will
// fail the build at the next `cargo test` regeneration (the
// `ReconciledKey` mapped type only compiles when every key is present).
// `id` is excluded because it is the map key itself; comparing it would
// be a tautology (`a.id === b.id` whenever both rows are valid). The
// `Omit<AgentNode, 'id'>` mapped type pins the *absence* of `id`, the
// `Record<..., true>` literal pins the *presence* of every other field.
type ReconciledKey = keyof Omit<AgentNode, 'id'>;
const AGENT_NODE_RECONCILE_SCHEMA: Record<ReconciledKey, true> = {
  mesh_id: true,
  name: true,
  path: true,
  branch: true,
  worktree_name: true,
  env: true,
  provider: true,
  status: true,
  cli_session_id: true,
  use_worktree: true,
  is_pinned: true,
  position: true,
  created_at: true,
  source_issue: true,
  source_pr: true,
  head_repo_owner: true,
  head_repo_clone_url: true,
  source_pr_pinned_sha: true,
  signal_health: true,
  worktree_path: true,
};
const AGENT_NODE_RECONCILE_FIELDS = Object.keys(
  AGENT_NODE_RECONCILE_SCHEMA,
) as ReadonlyArray<ReconciledKey>;

function shallowEqualAgentNode(a: AgentNode, b: AgentNode): boolean {
  for (const k of AGENT_NODE_RECONCILE_FIELDS) {
    if (a[k] !== b[k]) return false;
  }
  return true;
}

/// Apply a mesh's re-positioned nodes optimistically and persist them. The
/// updated nodes replace that mesh's entries; the whole id list is re-sorted
/// by (mesh_id, position) so the in-memory order matches `list_agent_nodes`.
/// On a backend error we resync from the DB rather than leave the UI out of
/// step.
async function persistPositions(
  set: (partial: Partial<AgentNodeState>) => void,
  get: () => AgentNodeState,
  updatedMeshNodes: AgentNode[],
) {
  // Compute the new id ordering (full list, mesh-sorted, position-sorted).
  // We rebuild both the map and the id list so subscribers that read either
  // see a consistent snapshot — the alternative (mutating only the affected
  // mesh's nodes) would leave a half-sorted state during the optimistic
  // window, which is invisible because the id list still references the same
  // node objects.
  const byId = new Map(updatedMeshNodes.map(n => [n.id, n]));
  const mergedNodes = get().nodeIds
    .map(id => get().nodesById[id])
    .filter(n => n !== undefined)
    .map(n => byId.get(n.id) ?? n)
    .sort((x, y) => x.mesh_id - y.mesh_id || x.position - y.position);
  const mergedById: Record<number, AgentNode> = {};
  const mergedIds: number[] = [];
  for (const n of mergedNodes) {
    mergedById[n.id] = n;
    mergedIds.push(n.id);
  }
  set({ nodesById: mergedById, nodeIds: mergedIds });
  try {
    const updates = updatedMeshNodes.map(n => [n.id, n.position] as [number, number]);
    await api.updateAgentNodePositions(updates);
  } catch (e) {
    set({ error: formatError(e) });
    await get().fetchAgentNodes();
  }
}

type WorktreeCloseActionResolver = (
  node: AgentNode,
  safety: WorktreeCloseSafety,
) => Promise<WorktreeCloseAction>;

const defaultWorktreeCloseActionResolver: WorktreeCloseActionResolver = (node, safety) =>
  requestWorktreeCloseAction(node.name, safety);

let worktreeCloseActionResolver = defaultWorktreeCloseActionResolver;

export function setWorktreeCloseActionResolverForTests(resolver?: WorktreeCloseActionResolver) {
  worktreeCloseActionResolver = resolver ?? defaultWorktreeCloseActionResolver;
}

// A pending "send input at time T" schedule (issue #785), e.g. the
// SchedulingPopover's "remind me in 5m" / "at usage reset" actions. Keyed by
// node ID in `schedules` — one active schedule per node.
export interface ScheduledTask {
  nodeId: number;
  targetTime: number;
  // Empty string is the "just hit Enter" sentinel — the SchedulingPopover uses
  // it for the "at usage reset" preset to wake the agent without new content.
  message: string;
  label: string;
  timeoutId: ReturnType<typeof setTimeout>;
}

export interface SpawnAgentOptions {
  rows?: number;
  cols?: number;
  fresh?: boolean;
  /// First-turn prompt (issue #1413). Non-empty wins over `fresh` /
  /// resume and becomes `SpawnAgentIntent.loop`.
  prefill?: string;
}

function resolveSpawnAgentIntent(
  options: SpawnAgentOptions,
  node: AgentNode | undefined,
): SpawnAgentIntent {
  const trimmed = options.prefill?.trim() ?? '';
  if (trimmed !== '') {
    return { type: 'loop', initial_prompt: trimmed };
  }
  if (options.fresh) {
    return { type: 'fresh' };
  }
  if (node?.cli_session_id) {
    return { type: 'resume' };
  }
  return { type: 'fresh' };
}

interface AgentNodeState {
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
  // Autopilot pipeline state per piloted node id ('implementing' /
  // 'finishing' / 'completed' / 'failed' / 'merged'). Absent key = not an
  // autopilot node. Drives the header's Autopilot pill; refreshed with the
  // node list and nudged by the `autopilot-*` lifecycle events.
  autopilotStates: Record<number, AutopilotRunState>;
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
  // Pending "send input at time T" schedules (issue #785), keyed by node ID.
  // One active schedule per node — a new `scheduleInput` cancels the prior
  // timer, and `deleteAgentNode` cancels the schedule outright so a stray
  // timeout can't fire `sendToAgent` against a deleted node. `fetchAgentNodes`
  // additionally cancels any schedule whose target node is absent from the
  // refreshed list OR has `status === 'archived'` (issue #1252) — otherwise
  // the autopilot-node-closed path would let a stale timer fire against an
  // archived node, surfacing a spurious "System" error toast minutes later.
  schedules: Record<number, ScheduledTask>;

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
  createAgentNode: (meshId: number, name: string, path: string, branch: string, provider?: string, useWorktree?: boolean) => Promise<AgentNode>;
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
  selectProviderForMesh: (meshId: number, meshName: string, meshPath: string, providerId: string, useWorktree?: boolean, initialPrompt?: string) => Promise<AgentNode>;
  deleteAgentNode: (id: number) => Promise<void>;
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
  clearAttention: (nodeId: number) => Promise<void>;
  // Issue #1054 — typed dispatch surface for `agentNodeListeners.ts`.
  // The listener module never reaches into `set`/`get` directly; it
  // dispatches through these actions (plus `fetchAgentNodes` /
  // `setActiveNode` from the public surface). Each action is a
  // one-liner that the store can also expose to other callers if a
  // future refactor needs the same seam.
  patchAgentNode: (id: number, patch: Partial<AgentNode>) => void;
  patchAutopilotState: (id: number, state: AutopilotRunState) => void;
  setSemanticTurn: (id: number, turn: SemanticTurnPayload | null) => void;
  findAgentNode: (id: number) => AgentNode | undefined;
  initAttentionListeners: () => Promise<void>;
  /// Schedule `message` (or a bare Enter if empty) to be sent to `nodeId`
  /// after `delayMs`. Replaces any existing schedule for the node — only one
  /// pending send per node at a time (issue #785).
  scheduleInput: (nodeId: number, delayMs: number, message: string, label: string) => void;
  cancelSchedule: (nodeId: number) => void;
}

/// Issue #1384 — derived selector for the full ordered node array. Components
/// that genuinely need the list (Sidebar, AgentNodeView, CommandOmnibar)
/// use this hook instead of the duplicated `useMemo(() => nodeIds.map(id =>
/// nodesById[id]).filter(...), [nodeIds, nodesById])` block. `useShallow`
/// does the shallow equality on the array's elements so unrelated writes
/// (autopilot pill, closing flag, error string) don't churn the consumer.
///
/// Returns a fresh array reference on every `nodeIds` change (e.g. a delete
/// or reorder), so `useMemo`-style downstream derivations in consumers
/// recompute correctly. Per-id selectors (`state.nodesById[id]`) are
/// preferred when the consumer only needs one node — they preserve identity
/// through the shallow reconciliation in `fetchAgentNodes`.
export function useAllAgentNodes(): AgentNode[] {
  return useAgentNodeStore(
    useShallow((s) =>
      s.nodeIds
        .map((id) => s.nodesById[id])
        .filter((n): n is AgentNode => n !== undefined),
    ),
  );
}

export const useAgentNodeStore = create<AgentNodeState>((set, get) => {
  // Issue #1054 — shared `OptimisticSurface` for the three sites
  // (`renameAgentNode`, `setNodePinned`, `toggleNodePinned`) that
  // route through `withOptimistic`. Built once per `create()` call so
  // the closures capture the right `set`/`get`. Functional updater
  // only — matches the helper's contract and Zustand's `set(updater)`
  // shape. Issue #1384 — the surface now operates per-node via the
  // normalized `nodesById` map; the helper writes a single node at a
  // time, never the whole list, so subscribers to other nodes are not
  // disturbed.
  const optimisticSurface: OptimisticSurface = {
    getAgentNode: (nodeId) => get().nodesById[nodeId],
    setAgentNode: (nodeId, next) => {
      set((state) => {
        const current = state.nodesById[nodeId];
        if (!current) return state;
        return {
          nodesById: { ...state.nodesById, [nodeId]: next(current) },
        };
      });
    },
    setError: (error) => set({ error }),
  };
  return {
  nodesById: {},
  nodeIds: [],
  autopilotStates: {},
  circuitOwnerships: {},
  semanticTurns: {},
  activeNodeId: null,
  loading: false,
  error: null,
  closingNodeIds: new Set(),
  schedules: {},

  getAgentNodes: () => {
    const { nodesById, nodeIds } = get();
    return nodeIds.map(id => nodesById[id]).filter((n): n is AgentNode => n !== undefined);
  },

  getActiveNode: () => {
    const { nodesById, activeNodeId } = get();
    if (activeNodeId === null) return null;
    return nodesById[activeNodeId] ?? null;
  },

  getActiveMeshId: () => {
    const node = get().getActiveNode();
    return node?.mesh_id ?? null;
  },

  fetchAgentNodes: async () => {
    set({ loading: true, error: null });
    try {
      // Autopilot states ride along with every node refresh, but their
      // failure must never blank the node list — degrade to "no pills".
      const [agentNodes, autopilotRuns, circuitOwnerships, semanticTurns] = await Promise.all([
        api.listAgentNodes(),
        api.listAutopilotRuns().catch(() => []),
        api.listCircuitAgentOwnerships().catch(() => Object.values(get().circuitOwnerships)),
        api.listSemanticTurns().catch(() => []),
      ]);
      const autopilotStates = Object.fromEntries(
        (Array.isArray(autopilotRuns) ? autopilotRuns : []).map((r) => [r.node_id, r.state]),
      );
      // Issue #1384 — shallow reconciliation. For each incoming node, check
      // against the existing entry under the same id; if all reconciled
      // fields match, keep the old object reference. The new `nodesById`
      // and `nodeIds` are always fresh containers (so Zustand subscribers
      // re-evaluate), but the per-id references they hold are the original
      // objects for unchanged nodes — `state.nodesById[id]` is the same
      // reference as before, and per-id selectors skip the render.
      const oldById = get().nodesById;
      const newById: Record<number, AgentNode> = {};
      const newIds: number[] = [];
      for (const n of agentNodes) {
        const old = oldById[n.id];
        newById[n.id] = old && shallowEqualAgentNode(old, n) ? old : n;
        newIds.push(n.id);
      }
      // Issue #1252 — a schedule that outlives its target node would
      // fire `send_to_agent` (or `write_to_agent` for the empty-message
      // "hit Enter" sentinel) at an archived node, the backend would
      // reject, and App.tsx's generic "System" toast pipeline would
      // surface a spurious error minutes after the user moved on. The
      // `autopilot-node-closed` listener (`stores/agentNodeListeners.ts`)
      // routes through here, so every archive transition sweeps
      // schedules for free — without this, the only cancellation path
      // was `deleteAgentNode`, which the archive path never invokes.
      //
      // Compute cancellations OUTSIDE the `set` updater — `clearTimeout`
      // is a side effect we don't want running twice under React
      // StrictMode. We keep the same object reference when nothing was
      // cancelled, so subscribers that read `state.schedules` don't
      // re-render on every fetch (the steady-state autopilot case).
      const oldSchedules = get().schedules;
      const keptSchedules: Record<number, ScheduledTask> = {};
      for (const idStr of Object.keys(oldSchedules)) {
        const id = Number(idStr);
        const node = newById[id];
        if (node && node.status !== 'archived') {
          keptSchedules[id] = oldSchedules[id];
        } else {
          clearTimeout(oldSchedules[id].timeoutId);
        }
      }
      const schedulesChanged =
        Object.keys(keptSchedules).length !== Object.keys(oldSchedules).length;
      set({
        nodesById: newById,
        nodeIds: newIds,
        autopilotStates,
        circuitOwnerships: Object.fromEntries(
          (Array.isArray(circuitOwnerships) ? circuitOwnerships : []).map((ownership) => [
            ownership.node_id,
            ownership,
          ]),
        ),
        semanticTurns: Object.fromEntries((Array.isArray(semanticTurns) ? semanticTurns : []).map((turn) => [turn.node_id, turn])),
        loading: false,
        ...(schedulesChanged && { schedules: keptSchedules }),
      });
    } catch (e) {
      set({ error: formatError(e), loading: false });
    }
  },

  // Issue #1054 — typed dispatch surface for `agentNodeListeners.ts`.
  // One-liners that the listeners dispatch through; also exposed on the
  // public surface in case future code wants the same seam. Issue #1384 —
  // now updates the single entry in `nodesById` (preserving identity for
  // every other node), rather than remapping the whole array.
  patchAgentNode: (id, patch) => {
    set((state) => {
      const current = state.nodesById[id];
      if (!current) return state;
      return {
        nodesById: { ...state.nodesById, [id]: { ...current, ...patch } },
      };
    });
  },
  patchAutopilotState: (id, state) => {
    set((s) => ({ autopilotStates: { ...s.autopilotStates, [id]: state } }));
  },
  findAgentNode: (id) => get().nodesById[id],

  ...(() => {
    let listenersAttached = false;
    return {
      initAttentionListeners: async () => {
        if (listenersAttached) return;
        listenersAttached = true;

        // Issue #1054 — the event-listener body moved to
        // `agentNodeListeners.ts`. The module owns the event-name →
        // action map; this site is now a one-line delegate. The
        // listener's returned unlisten handle is intentionally not
        // stored — the closure-guarded `listenersAttached` flag mirrors
        // the pre-refactor behaviour (React StrictMode's double-mount
        // short-circuits at the guard) and there is no component that
        // needs to detach the listeners (the store lives for the
        // lifetime of the webview).
        await attachAgentNodeListeners({
          fetchAgentNodes: get().fetchAgentNodes,
          setActiveNode: get().setActiveNode,
          patchAgentNode: get().patchAgentNode,
          patchAutopilotState: get().patchAutopilotState,
          setSemanticTurn: get().setSemanticTurn,
          findAgentNode: get().findAgentNode,
        });
      },
    };
  })(),

  createAgentNode: async (meshId, name, path, branch, provider?: string, useWorktree?: boolean): Promise<AgentNode> => {
    try {
      const node = await api.createAgentNode(meshId, name, path, branch, provider, useWorktree);
      set((state) => ({
        nodesById: { ...state.nodesById, [node.id]: node },
        nodeIds: [...state.nodeIds, node.id],
      }));
      return node;
    } catch (e) {
      set({ error: formatError(e) });
      throw e;
    }
  },

  selectProviderForMesh: async (meshId, meshName, meshPath, providerId, useWorktree?: boolean, initialPrompt?: string): Promise<AgentNode> => {
    // Create FIRST — only switch active mesh/node if creation succeeded.
    // The order is the invariant: pre-refactor this lived in three sequential
    // store calls in Sidebar.handleSelectProvider (#283), where a future hand
    // could re-arrange them and re-introduce the half-applied
    // "mesh selected but no node" state. Holding the order here makes the
    // invariant unit-testable and impossible to violate from a click handler.
    const node = await get().createAgentNode(meshId, meshName, meshPath, 'main', providerId, useWorktree);
    get().setActiveNode(node.id);
    useMeshStore.getState().selectMesh(meshId);
    // Explicit create-then-spawn (issue #1413 review): the prompt rides
    // on this call, not a store-side dictionary the Terminal mount
    // later peeks at. `spawnAgent` flips the row to `spawning`
    // synchronously so the idle auto-spawn effect cannot start a
    // parallel Fresh spawn. Whitespace-only is treated as "no prompt"
    // and Terminal auto-spawns as before.
    const trimmed = initialPrompt?.trim() ?? '';
    if (trimmed !== '') {
      await get().spawnAgent(node.id, providerId, { prefill: trimmed });
    }
    return node;
  },

  deleteAgentNode: async (id) => {
    // A close already in flight owns this node's teardown; a second click
    // (common while the safety check makes the row look unresponsive) must not
    // fire a duplicate safety/kill/delete round-trip.
    if (get().closingNodeIds.has(id)) return;

    // A scheduled send outlives the node it targets otherwise — cancel it
    // up front so a stray timeout can't fire `sendToAgent` for a deleted node.
    get().cancelSchedule(id);

    // Flag the node closing *synchronously* so the click registers instantly.
    // We can't drop the row yet — the safety check below decides whether to
    // prompt about uncommitted/unpushed work — so until it resolves NodeItem
    // shows a spinner over the still-present row instead of looking frozen.
    set((state) => ({ closingNodeIds: new Set(state.closingNodeIds).add(id) }));
    const clearClosing = () => set((state) => {
      const next = new Set(state.closingNodeIds);
      next.delete(id);
      return { closingNodeIds: next };
    });

    // Capture the row up-front so a Phase-2 (delete_commit) failure can
    // re-insert it. By the time the IPC rejects the row is already gone from
    // the store, so reading from get() would not find it.
    const node = get().nodesById[id];

    // Phase 1: safety check + worktree-confirmation prompt. Failures here
    // release the closing flag (the row is still on screen and retryable) and
    // surface `state.error` for the App.tsx toast pipeline. The promise
    // resolves — callers don't need to react to a transient safety-check
    // rejection any more than they do to the existing `kill_agent` warn-only.
    let removeWorktree = false;
    try {
      const safety = await api.getWorktreeCloseSafety(id);
      removeWorktree = Boolean(safety.worktree_path);

      if (safety.worktree_path && hasWorktreeCloseRisk(safety)) {
        if (!node) {
          throw new Error(`Agent node ${id} is not loaded, cannot confirm worktree removal`);
        }
        const action = await worktreeCloseActionResolver(node, safety);
        if (action === 'cancel') { clearClosing(); return; }
        removeWorktree = action === 'remove';
      }
    } catch (e) {
      clearClosing();
      set({ error: formatError(e) });
      return;
    }

    // Re-capture the row RIGHT BEFORE the optimistic remove so the restore
    // path below sees the version of the node that's actually being dropped
    // (Phase 1 awaited `getWorktree_close_safety`, and a `node-renamed`
    // event could fire during that window — capturing after the await closes
    // that race). Reading after the optimistic remove would find nothing.
    const nodeForRestore = get().nodesById[id];

    // Phase 2: optimistic close + backend commit. The row drops from the UI
    // here so closing feels instant; the backend kills the agent and removes
    // the row in a fast Phase 1, then reclaims the worktree directory in the
    // background (a failed cleanup surfaces later via the
    // 'worktree-cleanup-failed' event rather than holding the node on screen
    // while the slow delete runs).
    //
    // Issue #647: `disposeTerminal(id)` is intentionally NOT called yet.
    // Disposing here would blank the terminal before the delete IPC commits,
    // and on rejection the restored row (issue #645 zombie-row fix) would
    // re-mount an xterm bound to a dead PTY — the agent was killed by the
    // warn-only `kill_agent` path while `delete_agent_node` was still
    // pending. Disposal moves to AFTER the delete IPC succeeds below; on
    // rejection the terminal stays live so the user can retry without
    // losing scrollback. Trade-off: terminal lingers ~one IPC round-trip
    // longer on the happy path.
    set((state) => {
      const closing = new Set(state.closingNodeIds);
      closing.delete(id);
      if (!(id in state.nodesById)) return { closingNodeIds: closing };
      const nextById = { ...state.nodesById };
      delete nextById[id];
      // Issue #1384 — `semanticTurns` keyed by id, so the row's turn
      // metadata evaporates with the row on optimistic remove. The
      // listener re-keys when a node with the same id returns, but
      // id stability is enforced by the DB autoincrement.
      const semanticTurns = { ...state.semanticTurns };
      delete semanticTurns[id];
      return {
        nodesById: nextById,
        nodeIds: state.nodeIds.filter(nid => nid !== id),
        activeNodeId: state.activeNodeId === id ? null : state.activeNodeId,
        closingNodeIds: closing,
        semanticTurns,
      };
    });

    // kill_agent tears the process down before its bookkeeping (a DB status
    // update) can fail, and the node is being deleted anyway — so never let
    // a kill_agent rejection skip delete_agent_node, or the node would vanish
    // from the UI while its row and worktree survive and resurrect on the
    // next fetch.
    try {
      await api.killAgent(id);
    } catch (e) {
      console.warn('[agentNodeStore] kill_agent failed during close, continuing', e);
    }

    try {
      await api.deleteAgentNode(id, removeWorktree);
    } catch (e) {
      // Issue #645: restore the optimistically-removed row so UI/DB stay in
      // sync. Without this the catch would silently swallow the rejection,
      // and any unrelated fetchAgentNodes (node-created, mesh switch, …)
      // would resurrect the row anyway, making it look like a "zombie"
      // close. Re-inserting here makes the resurrection immediate. Re-throw
      // to match createAgentNode / renameAgentNode so callers awaiting the
      // close can react. `nodeForRestore` was captured AFTER Phase 1's
      // await so a `node-renamed` event fired mid-flight (e.g. user
      // renamed then closed) restores the post-rename version, not a stale
      // pre-rename snapshot.
      //
      // Issue #647: leave the terminal alive — the row is restored and
      // needs its scrollback + live PTY for the user to retry.
      //
      // Issue #1001: surface the failure through the shared toast pipeline.
      // The explicit toast is the single source of truth for delete
      // failures — `state.error` is intentionally NOT written here
      // because App.tsx subscribes to `useAgentNodeStore(state => state.error)`
      // for its generic "System" pipeline, and retaining it would produce
      // a duplicate "System" toast alongside the explicit "Node" toast
      // (different providers, the dedup wouldn't collapse them, two of
      // the three toast slots would burn on a single failure). Every
      // other `state.error` setter (Phase 1 worktree safety check, etc.)
      // continues to use the App.tsx System pipeline unchanged.
      if (nodeForRestore) {
        set((state) => {
          // Only restore if the row is still absent — a concurrent
          // `node-created` refetch may have already replaced it.
          if (state.nodesById[id]) return state;
          return {
            nodesById: { ...state.nodesById, [id]: nodeForRestore },
            nodeIds: state.nodeIds.includes(id) ? state.nodeIds : [...state.nodeIds, id],
          };
        });
      }
      addToast('Node', `Failed to close node: ${formatError(e)}`, 'error');
      throw e;
    }

    // Issue #647: only now — after the delete IPC committed — is the
    // terminal-persistence rule's "never dispose unless deleted" invariant
    // satisfied. See the Phase-2 NOTE above for the failure-path reasoning.
    disposeTerminal(id);
  },

  renameAgentNode: async (id, name) => {
    // Issue #1054 — precheck matches the pre-refactor behaviour: a
    // missing node is a silent no-op, not a throw. (The two pin
    // actions throw; the rename path was always a quiet return.)
    const prior = get().nodesById[id];
    if (!prior) return;
    // Optimistic update so the UI reflects the new name before the
    // round-trip. The backend emits `node-renamed` on success, which
    // is a no-op for us (already matches) and keeps other windows in
    // sync. We do NOT adopt the mutation's resolved value (rename
    // returns `void`) — that would race against the `node-renamed`
    // listener that arrives separately.
    await withOptimistic({
      surface: optimisticSurface,
      nodeId: id,
      optimisticPatch: { name },
      mutation: () => api.renameAgentNode(id, name),
    });
  },

  setNodePinned: async (nodeId, pinned) => {
    // Optimistic patch so the Pinned Grid view re-renders instantly on
    // a click. The backend's returned `AgentNode` is the source of truth
    // — a future refactor that mutates other columns on the way back
    // would otherwise be invisible. On rejection `withOptimistic`
    // reverts only the `is_pinned` column to its pre-call value, so a
    // concurrent update to any other column (e.g. status from the
    // orchestrator) is preserved.
    return withOptimistic({
      surface: optimisticSurface,
      nodeId,
      optimisticPatch: { is_pinned: pinned },
      mutation: () => api.setNodePinned(nodeId, pinned),
      adoptResult: (updated) => updated,
    });
  },

  toggleNodePinned: async (nodeId) => {
    // Optimistic flip — the visible state is `!prior.is_pinned` until
    // the backend confirms. Same source-of-truth-from-response pattern
    // as `setNodePinned`. Compute the flipped value up front so
    // `withOptimistic` has a single `optimisticPatch` to roll back;
    // the helper's `prior` capture handles the "node not loaded" throw.
    const prior = get().nodesById[nodeId];
    if (!prior) {
      throw new Error(`toggleNodePinned: node ${nodeId} is not loaded`);
    }
    return withOptimistic({
      surface: optimisticSurface,
      nodeId,
      optimisticPatch: { is_pinned: !prior.is_pinned },
      mutation: () => api.toggleNodePinned(nodeId),
      adoptResult: (updated) => updated,
    });
  },

  // Drag-to-reorder: move `nodeId` to flat `insertIndex` within its own mesh's
  // ordered list, renumber positions 0..n-1, and persist. Optimistic so the
  // grid rearranges instantly; a backend failure resyncs from the DB. Order is
  // mesh-scoped — other meshes' nodes are never touched (matches the
  // same-mesh-only drag guard in the UI). The merged array is re-sorted by
  // (mesh_id, position) to mirror `list_agent_nodes`' ordering exactly.
  reorderAgentNode: async (nodeId, insertIndex) => {
    const dragged = get().nodesById[nodeId];
    if (!dragged) return;
    const meshId = dragged.mesh_id;

    const meshNodes = get().nodeIds
      .map(id => get().nodesById[id])
      .filter((n): n is AgentNode => n !== undefined && n.mesh_id === meshId)
      .sort((a, b) => a.position - b.position);
    const from = meshNodes.findIndex(n => n.id === nodeId);
    if (from === -1) return;

    let to = insertIndex;
    meshNodes.splice(from, 1);
    if (from < to) to -= 1; // removing an earlier element shifts the target left
    to = Math.max(0, Math.min(to, meshNodes.length));
    meshNodes.splice(to, 0, dragged);
    if (from === to) return; // no-op (e.g. dropped on its own boundary)

    const renumbered = meshNodes.map((n, i) => ({ ...n, position: i }));
    await persistPositions(set, get, renumbered);
  },

  // Swap the grid slots of two nodes in the same mesh by exchanging their
  // `position` values. Only those two rows change, so we send just two updates.
  swapAgentNodes: async (aId, bId) => {
    if (aId === bId) return;
    const a = get().nodesById[aId];
    const b = get().nodesById[bId];
    if (!a || !b || a.mesh_id !== b.mesh_id) return;

    const swapped = get().nodeIds
      .map(id => get().nodesById[id])
      .filter((n): n is AgentNode => n !== undefined && n.mesh_id === a.mesh_id)
      .map(n => n.id === aId ? { ...n, position: b.position }
                : n.id === bId ? { ...n, position: a.position }
                : n);
    await persistPositions(set, get, swapped);
  },

  setActiveNode: (id) => {
    // Explicit navigation (including reselecting the same awaiting agent)
    // reveals its agent tab. Utility-tab clicks set their selection afterward.
    if (id !== null) {
      const state = get();
      useNodeActivityStore.getState().select(activityRootId(id, state.getAgentNodes(), state.circuitOwnerships), id);
    }
    // A plain synchronous state write — the active-node highlight, terminal
    // focus, and file-watch all key off activeNodeId, so the switch must feel
    // instant with no backend round-trip in the way.
    set({ activeNodeId: id });
  },

  spawnAgent: async (nodeId, provider, rowsOrOptions, maybeCols) => {
    const options: SpawnAgentOptions =
      typeof rowsOrOptions === 'object' && rowsOrOptions !== null
        ? rowsOrOptions
        : {
            rows: typeof rowsOrOptions === 'number' ? rowsOrOptions : undefined,
            cols: maybeCols,
          };
    const node = get().nodesById[nodeId];
    const previousStatus = node?.status;
    const intent = resolveSpawnAgentIntent(options, node);
    // Flip idle → spawning *before* the first await so Terminal's
    // auto-spawn effect (keyed on `status === 'idle'`) cannot start a
    // second spawn_agent while this one is in flight. Issue #1384 —
    // optimistic patch goes through `nodesById` so other entries keep
    // their identity.
    if (node?.status === 'idle') {
      set((state) => ({
        nodesById: {
          ...state.nodesById,
          [nodeId]: { ...state.nodesById[nodeId]!, status: 'spawning' },
        },
      }));
    }
    try {
      await api.spawnAgent({
        sessionId: nodeId,
        provider,
        intent,
        rows: options.rows ?? null,
        cols: options.cols ?? null,
      });
      await get().fetchAgentNodes();
    } catch (e) {
      // The central IPC wrapper (src/lib/tauri.ts → _invoke) already logs
      // every rejection to buildmesh.log as `[IPC:spawn_agent] args=… — <err>`,
      // so the prior `console.error` here produced a duplicate entry. The
      // store-side catch keeps doing two things the wrapper does not: it
      // surfaces the error on `state.error` for the UI to render, and it
      // re-throws so the caller's catch can react. Issue #1384 —
      // rollback through `nodesById` so other entries keep their identity.
      if (previousStatus !== undefined) {
        set((state) => ({
          nodesById: {
            ...state.nodesById,
            [nodeId]: { ...state.nodesById[nodeId]!, status: previousStatus },
          },
          error: formatError(e),
        }));
      } else {
        set({ error: formatError(e) });
      }
      throw e;
    }
  },

  regenerateAgentNode: async (nodeId, newProviderId) => {
    try {
      // The backend's returned `AgentNode` is the pre-spawn snapshot
      // (services::agent_node::regenerate reloads after the provider
      // UPDATE but BEFORE `spawn_agent_inner` mutates status / captures
      // `cli_session_id` / fires the `agent-spawned` event). A local
      // patch from that snapshot would clobber the event-driven state
      // — match `spawnAgent`'s pattern below and refetch instead. The
      // refetch reads the DB, which `spawn_agent_inner` has already
      // written, so the new `provider` / `cli_session_id` / `status`
      // all line up.
      await api.regenerateAgentNode(nodeId, newProviderId);
      await get().fetchAgentNodes();
      const updated = get().nodesById[nodeId];
      if (!updated) {
        throw new Error(`regenerate_agent_node: node ${nodeId} not found after refetch`);
      }
      return updated;
    } catch (e) {
      set({ error: formatError(e) });
      throw e;
    }
  },

  restartFreshAgent: async (nodeId, options) => {
    const node = get().nodesById[nodeId];
    if (!node) {
      throw new Error(`Node ${nodeId} not found`);
    }
    return get().spawnAgent(nodeId, node.provider, { ...options, fresh: true });
  },

  spawnHandoverAgent: async (meshId: number, prefill: string, provider?: string) => {
    try {
      const node = await api.spawnHandoverAgent(meshId, prefill, provider);
      set((state) => ({
        nodesById: { ...state.nodesById, [node.id]: node },
        nodeIds: state.nodeIds.includes(node.id) ? state.nodeIds : [...state.nodeIds, node.id],
      }));
      await get().fetchAgentNodes();
      return node;
    } catch (e) {
      set({ error: formatError(e) });
      throw e;
    }
  },

  killAgent: async (nodeId) => {
    try {
      await api.killAgent(nodeId);
      await get().fetchAgentNodes();
    } catch (e) {
      set({ error: formatError(e) });
    }
  },

  sendToAgent: async (nodeId, input) => {
    try {
      await api.sendToAgent(nodeId, input);
    } catch (e) {
      set({ error: formatError(e) });
    }
  },

  writeToAgent: async (nodeId, data) => {
    try {
      await api.writeToAgent(nodeId, data);
    } catch (e) {
      set({ error: formatError(e) });
    }
  },

  clearAttention: async (nodeId) => {
    try { await api.clearAttentionNode(nodeId); } catch (e) { set({ error: formatError(e) }); }
  },

  setSemanticTurn: (id, turn) => {
    set((state) => {
      const semanticTurns = { ...state.semanticTurns };
      if (turn) semanticTurns[id] = turn;
      else delete semanticTurns[id];
      return { semanticTurns };
    });
  },

  scheduleInput: (nodeId, delayMs, message, label) => {
    get().cancelSchedule(nodeId);

    const timeoutId = setTimeout(() => {
      set((state) => {
        const next = { ...state.schedules };
        delete next[nodeId];
        return { schedules: next };
      });
      // Issue #1252 — belt-and-braces guard. The fetch-time cancellation
      // in `fetchAgentNodes` is the common path: an `autopilot-node-
      // closed` event archives the node, the listener refetches, and
      // any pending schedule is dropped. This guard covers the narrow
      // race where the timer resolves AFTER the schedule was created
      // but BEFORE the refetch lands — bail silently rather than call
      // `send_to_agent` on a node the backend would reject.
      const node = get().findAgentNode(nodeId);
      if (!node || node.status === 'archived') return;
      if (message === '') {
        get().writeToAgent(nodeId, '\n');
      } else {
        get().sendToAgent(nodeId, message);
      }
    }, delayMs);

    set((state) => ({
      schedules: {
        ...state.schedules,
        [nodeId]: { nodeId, targetTime: Date.now() + delayMs, message, label, timeoutId },
      },
    }));
  },

  cancelSchedule: (nodeId) => {
    const existing = get().schedules[nodeId];
    if (!existing) return;
    clearTimeout(existing.timeoutId);
    set((state) => {
      const next = { ...state.schedules };
      delete next[nodeId];
      return { schedules: next };
    });
  },
  };
});
