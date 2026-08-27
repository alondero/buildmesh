import { formatError } from '../lib/errorUtils';
import { create } from 'zustand';
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

/// Apply a mesh's re-positioned nodes optimistically and persist them. The
/// updated nodes replace that mesh's entries; the whole array is re-sorted by
/// (mesh_id, position) so the in-memory order matches `list_agent_nodes`. On a
/// backend error we resync from the DB rather than leave the UI out of step.
async function persistPositions(
  set: (partial: Partial<AgentNodeState>) => void,
  get: () => AgentNodeState,
  updatedMeshNodes: AgentNode[],
) {
  const byId = new Map(updatedMeshNodes.map(n => [n.id, n]));
  const merged = get().agentNodes
    .map(n => byId.get(n.id) ?? n)
    .sort((x, y) => x.mesh_id - y.mesh_id || x.position - y.position);
  set({ agentNodes: merged });
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
}

interface AgentNodeState {
  agentNodes: AgentNode[];
  // Autopilot pipeline state per piloted node id ('implementing' /
  // 'finishing' / 'completed' / 'failed' / 'merged'). Absent key = not an
  // autopilot node. Drives the header's Autopilot pill; refreshed with the
  // node list and nudged by the `autopilot-*` lifecycle events.
  autopilotStates: Record<number, AutopilotRunState>;
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
  getActiveNode: () => AgentNode | null;
  getActiveMeshId: () => number | null;

  fetchAgentNodes: () => Promise<void>;
  createAgentNode: (meshId: number, name: string, path: string, branch: string, provider?: string, useWorktree?: boolean) => Promise<AgentNode>;
  /// Sidebar "click + or pick provider" entrypoint — creates a node on the
  /// mesh, sets it active, and selects the mesh. The three steps live behind
  /// one action (issue #283) so the invariant — "only switch active mesh/node
  /// if creation succeeded" — is enforced in one place, not re-derivable per
  /// click handler. On `createAgentNode` rejection the active mesh stays put.
  selectProviderForMesh: (meshId: number, meshName: string, meshPath: string, providerId: string, useWorktree?: boolean) => Promise<AgentNode>;
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
  // Issue #1054 — typed dispatch surface for `agentNodeListeners.ts`.
  // The listener module never reaches into `set`/`get` directly; it
  // dispatches through these actions (plus `fetchAgentNodes` /
  // `setActiveNode` from the public surface). Each action is a
  // one-liner that the store can also expose to other callers if a
  // future refactor needs the same seam.
  patchAgentNode: (id: number, patch: Partial<AgentNode>) => void;
  patchAutopilotState: (id: number, state: AutopilotRunState) => void;
  findAgentNode: (id: number) => AgentNode | undefined;
  initAttentionListeners: () => Promise<void>;
  /// Schedule `message` (or a bare Enter if empty) to be sent to `nodeId`
  /// after `delayMs`. Replaces any existing schedule for the node — only one
  /// pending send per node at a time (issue #785).
  scheduleInput: (nodeId: number, delayMs: number, message: string, label: string) => void;
  cancelSchedule: (nodeId: number) => void;
}

export const useAgentNodeStore = create<AgentNodeState>((set, get) => {
  // Issue #1054 — shared `OptimisticSurface` for the three sites
  // (`renameAgentNode`, `setNodePinned`, `toggleNodePinned`) that
  // route through `withOptimistic`. Built once per `create()` call so
  // the closures capture the right `set`/`get`. Functional updater
  // only — matches the helper's contract and Zustand's `set(updater)`
  // shape.
  const optimisticSurface: OptimisticSurface = {
    getAgentNodes: () => get().agentNodes,
    setAgentNodes: (updater) =>
      set((state) => ({ agentNodes: updater(state.agentNodes) })),
    setError: (error) => set({ error }),
  };
  return {
  agentNodes: [],
  autopilotStates: {},
  activeNodeId: null,
  loading: false,
  error: null,
  closingNodeIds: new Set(),
  schedules: {},

  getActiveNode: () => {
    const { agentNodes, activeNodeId } = get();
    if (activeNodeId === null) return null;
    return agentNodes.find(s => s.id === activeNodeId) || null;
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
      const [agentNodes, autopilotRuns] = await Promise.all([
        api.listAgentNodes(),
        api.listAutopilotRuns().catch(() => []),
      ]);
      const autopilotStates = Object.fromEntries(
        (Array.isArray(autopilotRuns) ? autopilotRuns : []).map((r) => [r.node_id, r.state]),
      );
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
        const node = agentNodes.find((n) => n.id === id);
        if (node && node.status !== 'archived') {
          keptSchedules[id] = oldSchedules[id];
        } else {
          clearTimeout(oldSchedules[id].timeoutId);
        }
      }
      const schedulesChanged =
        Object.keys(keptSchedules).length !== Object.keys(oldSchedules).length;
      set({
        agentNodes,
        autopilotStates,
        loading: false,
        ...(schedulesChanged && { schedules: keptSchedules }),
      });
    } catch (e) {
      set({ error: formatError(e), loading: false });
    }
  },

  // Issue #1054 — typed dispatch surface for `agentNodeListeners.ts`.
  // One-liners that the listeners dispatch through; also exposed on the
  // public surface in case future code wants the same seam.
  patchAgentNode: (id, patch) => {
    set((state) => ({
      agentNodes: state.agentNodes.map((s) =>
        s.id === id ? { ...s, ...patch } : s
      ),
    }));
  },
  patchAutopilotState: (id, state) => {
    set((s) => ({ autopilotStates: { ...s.autopilotStates, [id]: state } }));
  },
  findAgentNode: (id) => get().agentNodes.find((s) => s.id === id),

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
          findAgentNode: get().findAgentNode,
        });
      },
    };
  })(),

  createAgentNode: async (meshId, name, path, branch, provider?: string, useWorktree?: boolean): Promise<AgentNode> => {
    try {
      const node = await api.createAgentNode(meshId, name, path, branch, provider, useWorktree);
      set((state) => ({ agentNodes: [...state.agentNodes, node] }));
      return node;
    } catch (e) {
      set({ error: formatError(e) });
      throw e;
    }
  },

  selectProviderForMesh: async (meshId, meshName, meshPath, providerId, useWorktree?: boolean): Promise<AgentNode> => {
    // Create FIRST — only switch active mesh/node if creation succeeded.
    // The order is the invariant: pre-refactor this lived in three sequential
    // store calls in Sidebar.handleSelectProvider (#283), where a future hand
    // could re-arrange them and re-introduce the half-applied
    // "mesh selected but no node" state. Holding the order here makes the
    // invariant unit-testable and impossible to violate from a click handler.
    const node = await get().createAgentNode(meshId, meshName, meshPath, 'main', providerId, useWorktree);
    get().setActiveNode(node.id);
    useMeshStore.getState().selectMesh(meshId);
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
    // `agentNodes`, so reading from get() would not find it.
    const node = get().agentNodes.find(s => s.id === id);

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
    const nodeForRestore = get().agentNodes.find(s => s.id === id);

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
      return {
        agentNodes: state.agentNodes.filter(s => s.id !== id),
        activeNodeId: state.activeNodeId === id ? null : state.activeNodeId,
        closingNodeIds: closing,
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
      set((state) => ({
        agentNodes: nodeForRestore ? [...state.agentNodes, nodeForRestore] : state.agentNodes,
      }));
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
    const prior = get().agentNodes.find(s => s.id === id);
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
    const prior = get().agentNodes.find(s => s.id === nodeId);
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
    const dragged = get().agentNodes.find(n => n.id === nodeId);
    if (!dragged) return;
    const meshId = dragged.mesh_id;

    const meshNodes = get().agentNodes
      .filter(n => n.mesh_id === meshId)
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
    const a = get().agentNodes.find(n => n.id === aId);
    const b = get().agentNodes.find(n => n.id === bId);
    if (!a || !b || a.mesh_id !== b.mesh_id) return;

    const swapped = get().agentNodes
      .filter(n => n.mesh_id === a.mesh_id)
      .map(n => n.id === aId ? { ...n, position: b.position }
                : n.id === bId ? { ...n, position: a.position }
                : n);
    await persistPositions(set, get, swapped);
  },

  setActiveNode: (id) => {
    // A plain synchronous state write — the active-node highlight, terminal
    // focus, and file-watch all key off activeNodeId, so the switch must feel
    // instant with no backend round-trip in the way.
    set({ activeNodeId: id });
  },

  spawnAgent: async (nodeId, provider, rowsOrOptions, maybeCols) => {
    try {
      const options: SpawnAgentOptions =
        typeof rowsOrOptions === 'object' && rowsOrOptions !== null
          ? rowsOrOptions
          : {
              rows: typeof rowsOrOptions === 'number' ? rowsOrOptions : undefined,
              cols: maybeCols,
            };
      const node = get().agentNodes.find(s => s.id === nodeId);
      const resume = options.fresh ? null : (node?.cli_session_id ?? null);
      await api.spawnAgent(nodeId, provider, resume, options.rows, options.cols);
      await get().fetchAgentNodes();
    } catch (e) {
      // The central IPC wrapper (src/lib/tauri.ts → _invoke) already logs
      // every rejection to buildmesh.log as `[IPC:spawn_agent] args=… — <err>`,
      // so the prior `console.error` here produced a duplicate entry. The
      // store-side catch keeps doing two things the wrapper does not: it
      // surfaces the error on `state.error` for the UI to render, and it
      // re-throws so the caller's catch can react.
      set({ error: formatError(e) });
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
      const updated = get().agentNodes.find((n) => n.id === nodeId);
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
    const node = get().agentNodes.find(s => s.id === nodeId);
    if (!node) {
      throw new Error(`Node ${nodeId} not found`);
    }
    return get().spawnAgent(nodeId, node.provider, { ...options, fresh: true });
  },

  spawnHandoverAgent: async (meshId: number, prefill: string, provider?: string) => {
    try {
      const node = await api.spawnHandoverAgent(meshId, prefill, provider);
      set((state) => ({ agentNodes: [...state.agentNodes, node] }));
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
