import type { AgentNodeState, SetAgentNodeState, GetAgentNodeState, AgentNodeRuntime } from './types';
import type { AgentNode } from '../../types/generated/AgentNode';
import { formatError } from '../../lib/errorUtils';
import * as api from '../../lib/tauri/agentNode';
import { killAgent, cancelTerminalInput } from '../../lib/tauri/agent';
import { disposeTerminal } from '../../components/Terminal/Terminal';
import { hasWorktreeCloseRisk, type WorktreeCloseAction, type WorktreeCloseSafety } from '../../lib/worktreeClose';
import { requestWorktreeCloseAction, useWorktreeClosePromptStore } from '../worktreeClosePromptStore';
import { isActiveForExit } from '../../lib/exitGuard';
import { addToast } from '../toastStore';
import type { OptimisticFieldLedger } from '../../lib/optimistic';

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

/// Issue #2242 — the row-removal transition, shared by the two paths that drop
/// a node: the user close (`deleteAgentNode`, optimistically, before the
/// delete IPC commits) and the circuit-driven reconcile (`removeAgentNode`).
///
/// Both must evaporate the row from the SAME set of containers. A row left in
/// `nodeIds` but missing from `nodesById` reappears on the next derivation;
/// a row left in `activeNodeId` points at nothing; and issue #1384's
/// `semanticTurns` is keyed by id, so turn metadata that outlives the row
/// re-attaches to whatever id the DB hands out next. Keeping the four writes
/// in one function makes that a property of the transition rather than
/// something two call sites have to remember to keep in step.
///
/// Returns `null` when the store does not hold the row: the optimistic close
/// and the reconcile can both race a fetch that already dropped it, and
/// neither may resurrect it by writing stale containers.
type RowRemoval = Pick<
  AgentNodeState,
  'nodesById' | 'nodeIds' | 'activeNodeId' | 'semanticTurns'
>;

function withoutRow(state: AgentNodeState, id: number): RowRemoval | null {
  if (!(id in state.nodesById)) return null;
  const nodesById = { ...state.nodesById };
  delete nodesById[id];
  // Issue #1384 — `semanticTurns` keyed by id, so the row's turn metadata
  // evaporates with the row. The listener re-keys when a node with the same
  // id returns, but id stability is enforced by the DB autoincrement.
  const semanticTurns = { ...state.semanticTurns };
  delete semanticTurns[id];
  return {
    nodesById,
    nodeIds: state.nodeIds.filter((nodeId) => nodeId !== id),
    activeNodeId: state.activeNodeId === id ? null : state.activeNodeId,
    semanticTurns,
  };
}


export function createCloseSlice(set: SetAgentNodeState, get: GetAgentNodeState, runtime: AgentNodeRuntime, optimisticLedger: OptimisticFieldLedger): Pick<AgentNodeState, 'deleteAgentNode' | 'removeAgentNode'> {
  return {
  deleteAgentNode: async (id) => {
    // A close already in flight owns this node's teardown; a second click
    // (common while the safety check makes the row look unresponsive) must not
    // fire a duplicate safety/kill/delete round-trip.
    if (get().closingNodeIds.has(id)) return;

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
      } else if (!safety.worktree_path) {
        const liveNode = get().nodesById[id];
        if (liveNode && isActiveForExit(liveNode.status)) {
          const action = await useWorktreeClosePromptStore.getState().request(liveNode.name, safety, 'active-session');
          if (action === 'cancel') { clearClosing(); return; }
        }
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
      const removed = withoutRow(state, id);
      return removed ? { ...removed, closingNodeIds: closing } : { closingNodeIds: closing };
    });
    // Issue #2325 — the row is gone, so any mutation still in flight for it
    // owns nothing and must not write it back when it settles.
    optimisticLedger.forget(id);

    // kill_agent tears the process down before its bookkeeping (a DB status
    // update) can fail, and the node is being deleted anyway — so never let
    // a kill_agent rejection skip delete_agent_node, or the node would vanish
    // from the UI while its row and worktree survive and resurrect on the
    // next fetch.
    //
    // Issue #1530: this calls `killAgent` directly, so it does not pass
    // through the `killAgent` action that releases the transport's retry lane.
    // Release it here, next to the kill that makes the buffered bytes
    // undeliverable. Deliberately *not* at the top of this function: the
    // worktree-safety prompt above can still be cancelled, and dropping a live
    // node's in-flight keystrokes because the user opened a close dialog would
    // trade one silent loss for another.
    try {
      await killAgent(id);
      cancelTerminalInput(id);
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

  removeAgentNode: (id) => {
    runtime.spawnRequests.delete(id);
    runtime.failedAutoSpawns.delete(id);
    // Issue #2325 — same reasoning as the close path: a retired row owns
    // no field, so a mutation still in flight cannot write it back.
    optimisticLedger.forget(id);
    // A user-initiated close already dropped the row optimistically and owns
    // its terminal disposal; only act when the store still holds the row.
    if (!(id in get().nodesById)) return;
    set((state) => withoutRow(state, id) ?? state);
    // The backend row is gone — the only case the terminal-persistence rule
    // allows disposal. This is the circuit-driven counterpart of
    // `deleteAgentNode`'s success-path dispose.
    disposeTerminal(id);
  },

  };
}
