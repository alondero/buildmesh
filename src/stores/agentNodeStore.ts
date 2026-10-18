import { createCloseSlice } from './agentNode/closeSlice';
import { createMutationSlice } from './agentNode/mutationSlice';
import { createLifecycleSlice } from './agentNode/lifecycleSlice';
import { createCreationSlice } from './agentNode/creationSlice';
import { shallowEqualAgentNode, reconcileOwnerships } from './agentNode/reconciliation';
import type { AgentNodeState, AgentNodeRuntime } from './agentNode/types';
export type { SpawnAgentOptions } from './agentNode/types';
export { setWorktreeCloseActionResolverForTests } from './agentNode/closeSlice';
import { formatError } from '../lib/errorUtils';
import { create } from 'zustand';
import * as api from '../lib/tauri';
import {
  createOptimisticFieldLedger,
  type AgentNodeField,
  type OptimisticSurface,
} from '../lib/optimistic';
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
export type { AgentNode };

/// Single-flight plus one trailing re-run, shared by `fetchAgentNodes` and
/// `refreshCircuitOwnerships`. A burst of callers collapses to the in-flight
/// read plus exactly one follow-up, and the follow-up is what lands last — so
/// the newest request's result is the one that commits. The trailing run is
/// chained through a rejection handler as well as the fulfilment path: a
/// transient failure of the in-flight task must not swallow the queued retry.
function createCoalescedRefresh(task: () => Promise<void>): () => Promise<void> {
  let inFlight: Promise<void> | null = null;
  let queued: Promise<void> | null = null;
  const run = (): Promise<void> => {
    if (inFlight) {
      if (!queued) {
        const current = inFlight;
        const afterInFlight = () => {
          queued = null;
          return run();
        };
        queued = current.then(afterInFlight, afterInFlight);
      }
      return queued;
    }
    const tracked = task().finally(() => {
      if (inFlight === tracked) inFlight = null;
    });
    inFlight = tracked;
    return tracked;
  };
  return run;
}

// Issue #1524 — test seam for the listener attachment state machine
// (`idle -> attaching -> attached`) that `initAttentionListeners` closes
// over. The state lives in the store's `create()` callback, so it is one
// instance per test file: without a reset, the first test to attach would
// quietly become every later test's precondition, and a test run on its
// own would exercise a different path than the same test in a full file.
//
// This does NOT detach listeners that are already registered — production
// has no teardown point (the store outlives the webview's components), and
// in tests the `listen` mock belongs to the test that installed it. It only
// clears the bookkeeping so the next `initAttentionListeners()` starts a
// fresh attempt.
let resetAgentNodeListenerAttachment: (() => void) | null = null;

export function resetAgentNodeListenersForTests(): void {
  resetAgentNodeListenerAttachment?.();
}

/// Issue #2021 — the derived full node array, memoized once per store
/// notification instead of once per subscriber.
///
/// Before this, `useAllAgentNodes` ran `nodeIds.map(...).filter(...)` inside
/// every subscriber's selector, so one store notification allocated and
/// scanned the full array *once per mounted consumer* — seven times in the
/// real app (Sidebar, AttentionList, AgentHistoryTab, CommandOmnibar,
/// ScopeIndicator, GridFilterPopover, AgentNodeView). Profiling at 100/500/
/// 1000 nodes showed the per-notification allocation growing linearly with
/// node count for every subscriber, and each consumer's O(N) grouping memo
/// re-running on every unrelated single-node write.
///
/// The memo is keyed on the identity of the two normalized containers.
/// Every write that can change a node replaces `nodesById` (and a reorder or
/// delete replaces `nodeIds`), so a changed reference means the derivation must
/// re-run; an unchanged pair means nothing that feeds it moved, and the cached
/// array is returned as-is. Writes that do NOT touch nodes — a Circuit
/// ownership patch, `loading`, `error`, `stalledInputs` — keep both
/// references identical and now cost one `Object.is` pair instead of N
/// dereferences per subscriber.
///
/// Returning the *previous* array when the freshly derived one is element-wise
/// identical preserves the guarantee `useShallow` used to provide: a
/// reconciliation that changed no field (a plain `fetchAgentNodes` poll where
/// every node compares equal) leaves the consumer's array reference stable and
/// skips the render. Element identity comes from `shallowEqualAgentNode`, so
/// this is an element-reference comparison, not a field comparison.
///
/// Not thread- or render-safe to share across *different* store instances —
/// `tests/unit/helpers/seedAgentNodes` and tests that `setState` directly rely
/// on picking up a new derivation. Keying on both container references handles
/// that: a reseed replaces both.
///
/// The returned array is SHARED by every subscriber, where the previous
/// `useShallow` implementation handed each subscriber its own. No consumer
/// mutates it today (they sort a `.filter()`/`.map()` copy), but a future
/// in-place `agentNodes.sort(...)` would corrupt every subscriber and the
/// cache. Consumers that need to reorder must copy first.
let derivedNodesCache: {
  nodeIds: readonly number[];
  nodesById: Record<number, AgentNode>;
  nodes: readonly AgentNode[];
} | null = null;

function deriveAgentNodes(
  nodeIds: readonly number[],
  nodesById: Record<number, AgentNode>,
): readonly AgentNode[] {
  const cached = derivedNodesCache;
  if (cached && cached.nodeIds === nodeIds && cached.nodesById === nodesById) {
    return cached.nodes;
  }
  const derived: AgentNode[] = [];
  for (const id of nodeIds) {
    const node = nodesById[id];
    if (node !== undefined) derived.push(node);
  }
  // Preserve `useShallow`'s referential stability: a derivation whose elements
  // are all the same references as the previous one keeps the previous array.
  const previous = cached?.nodes;
  if (previous && previous.length === derived.length) {
    let identical = true;
    for (let i = 0; i < derived.length; i += 1) {
      if (previous[i] !== derived[i]) {
        identical = false;
        break;
      }
    }
    if (identical) {
      derivedNodesCache = { nodeIds, nodesById, nodes: previous };
      return previous;
    }
  }
  derivedNodesCache = { nodeIds, nodesById, nodes: derived };
  return derived;
}

/// Issue #1384 — derived selector for the full ordered node array. Components
/// that genuinely need the list (Sidebar, AgentNodeView, CommandOmnibar)
/// use this hook instead of the duplicated `useMemo(() => nodeIds.map(id =>
/// nodesById[id]).filter(...), [nodeIds, nodesById])` block.
///
/// Issue #2021 — the derivation is memoized in [`deriveAgentNodes`] and
/// already returns a reference-stable array, so this hook no longer needs
/// `useShallow`'s element walk. Plain `Object.is` against the store slice is
/// now sufficient, and a subscriber that mounts *after* a notification still
/// receives the current array rather than a re-derived one.
///
/// Returns a fresh array reference whenever any node actually changed (e.g. a
/// delete, a reorder, a status flip), so `useMemo`-style downstream
/// derivations in consumers recompute correctly. Per-id selectors
/// (`state.nodesById[id]`) are preferred when the consumer only needs one node
/// — they preserve identity through the shallow reconciliation in
/// `fetchAgentNodes`.
///
/// The result is `readonly` because every subscriber shares ONE instance (see
/// [`deriveAgentNodes`]). That turns the shared-cache invariant into something
/// the compiler enforces rather than a convention: sorting or reversing this
/// array in place is a type error, where it would otherwise corrupt every
/// subscriber and the cache without triggering a React update. Consumers that
/// need their own order copy first — `[...nodes].sort(...)`.
export function useAllAgentNodes(): readonly AgentNode[] {
  return useAgentNodeStore((s) => deriveAgentNodes(s.nodeIds, s.nodesById));
}

/// Freshness window for `refreshIfStale` (issue #1751). A skip only
/// ever collapses a redundant back-to-back fetch (an event burst lands
/// within milliseconds of the snapshot it follows); handlers still
/// apply the event's own transition unconditionally first, so skipping
/// the fan-out can never leave newer event state unapplied. A fetch
/// failure does not refresh the stamp, so the next event retries.
export const REFRESH_IF_STALE_MS = 10_000;

export const useAgentNodeStore = create<AgentNodeState>((set, get) => {
  // Monotonic revision of the ownership ledger. Every newer authoritative
  // write bumps it: a `circuit-run-updated` patch, or a full `fetchAgentNodes`
  // snapshot. A satellite read captures it before awaiting and drops its
  // response when it moved — the read started earlier, so its snapshot is
  // older than the state already applied. Coalescing is not sufficient on its
  // own: it orders the queue, but a stale in-flight response still lands after
  // the newer event (spec `circuit-node-indicators.md` step 7: "an older
  // response cannot overwrite newer event state"). Issue #2325 — an optimistic
  // mutation's writes count too (see `optimisticLedger`'s `onFieldWrite`), so
  // a read that began before an acknowledged pin or rename is discarded and
  // re-read rather than painted over it.
  let circuitOwnershipsRevision = 0;
  const runtime: AgentNodeRuntime = { nodeRevision: 0, spawnRequests: new Map(), failedAutoSpawns: new Set() };
  // Wall-clock ms of the last successful `fetchAgentNodes` snapshot.
  // `refreshIfStale` reads it; only a success stamps it (a rejection
  // keeps the previous stamp so the next event retries). `0` until the
  // first success, so the first scoped check always fetches.
  let lastFetchedAtMs = 0;
  // Issue #1054 — shared `OptimisticSurface` for the three sites
  // (`renameAgentNode`, `setNodePinned`, `toggleNodePinned`) that
  // route through `withOptimistic`. Built once per `create()` call so
  // the closures capture the right `set`/`get`. Functional updater
  // only — matches the helper's contract and Zustand's `set(updater)`
  // shape. Issue #1384 — the surface now operates per-node via the
  // normalized `nodesById` map; the helper writes a single node at a
  // time, never the whole list, so subscribers to other nodes are not
  // disturbed. Issue #2325 — the ledger lives beside it rather than in
  // the helper module: it is per-store state, and every store (and every
  // test) must start with no claims.
  const optimisticLedger = createOptimisticFieldLedger({
    // An optimistic write — the patch itself, or the reconciliation of
    // its columns — is a newer write than any `list_agent_nodes`
    // snapshot that began before it, so the snapshot fence has to see
    // it. Without this an acknowledged pin or rename was silently
    // reverted by an in-flight read, which then discarded itself and
    // re-read: the fence existed but did not observe optimistic writes.
    onFieldWrite: () => { runtime.nodeRevision += 1; },
  });
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

  // Serialize full refreshes: concurrent callers share one trailing refresh
  // instead of opening competing IPC/database snapshots.
  const fetchAgentNodes = createCoalescedRefresh(async () => {
    // Sample the ledger revision BEFORE awaiting. A live `circuit-run-updated`
    // can land while this snapshot is in flight; its patch is newer than the
    // result about to arrive, so the ownership slice below must be dropped
    // rather than written over it. Dropping (and not bumping) also keeps the
    // event's trailing satellite read alive: bumping here would make that read
    // look superseded and it would discard the newest state, leaving the stale
    // snapshot in place — the exact regression this guard exists to prevent.
    const ownershipRevision = circuitOwnershipsRevision;
    const observedNodeRevision = runtime.nodeRevision;
    set({ loading: true, error: null });
    try {
      // Satellite reads fail independently. A transient failure preserves
      // the last known satellite state instead of clearing a live path.
      const [agentNodes, circuitOwnerships, semanticTurns] = await Promise.all([
        api.listAgentNodes(),
        api.listCircuitAgentOwnerships().catch(() => null),
        api.listSemanticTurns().catch(() => null),
      ]);
      if (observedNodeRevision !== runtime.nodeRevision) {
        // A live lifecycle event superseded this read. Queue one fresh snapshot
        // instead of briefly painting the old status and request over it.
        void fetchAgentNodes();
        return;
      }
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
        // A missing failure event can leave the backend snapshot idle. Only
        // an explicit spawn or a live lifecycle transition clears this fence.
        const incoming = n.status === 'idle' && old && runtime.spawnRequests.has(n.id)
          ? { ...n, status: old.status }
          : old?.status === 'error' && runtime.failedAutoSpawns.has(n.id) && n.status === 'idle'
            ? { ...n, status: 'error' as const } : n;
        newById[n.id] = old && shallowEqualAgentNode(old, incoming) ? old : incoming;
        newIds.push(n.id);
      }
      for (const id of runtime.failedAutoSpawns) {
        if (!newById[id] || newById[id].status === 'archived') runtime.failedAutoSpawns.delete(id);
      }
      // A live event patched the ledger while this snapshot was in flight, so
      // this ownership list is older than what the store already holds. Keep
      // the newer state and leave the revision alone — the event's own
      // trailing satellite read is the one entitled to commit here.
      const ownershipsStale = ownershipRevision !== circuitOwnershipsRevision;
      const reconciledOwnerships = Array.isArray(circuitOwnerships) && !ownershipsStale
        ? reconcileOwnerships(get().circuitOwnerships, circuitOwnerships).ownerships
        : get().circuitOwnerships;
      // Only a snapshot that is actually applied counts as a newer write for
      // in-flight satellite reads to defer to.
      if (!ownershipsStale) circuitOwnershipsRevision += 1;
      set({
        nodesById: newById,
        nodeIds: newIds,
        circuitOwnerships: reconciledOwnerships,
        semanticTurns: !Array.isArray(semanticTurns)
          ? get().semanticTurns
          : Object.fromEntries(semanticTurns.map((turn) => [turn.node_id, turn])),
        loading: false,
      });
      // Success stamps the freshness clock `refreshIfStale` reads. A
      // rejection below skips the stamp so the next event retries rather
      // than treating a failed snapshot as fresh.
      lastFetchedAtMs = Date.now();
    } catch (e) {
      set({ error: formatError(e), loading: false });
    }
  });

  // Re-read just the Circuit ownership ledger. `patchCircuitOwnershipState`
  // can only rewrite rows the map already holds (it matches by `run_id`), so a
  // `circuit-run-updated` event — which carries only `{ run_id, state }` —
  // cannot introduce an ownership row the UI has not seen: a node's first run,
  // or a fresh run on a node whose prior ownership is terminal. Without this
  // read the node glyph's Circuit ring and the title bar's "already under review" control
  // stay blank until an unrelated full `fetchAgentNodes` (the next node spawn).
  const refreshCircuitOwnerships = createCoalescedRefresh(async () => {
    const revision = circuitOwnershipsRevision;
    try {
      const rows = await api.listCircuitAgentOwnerships();
      // A newer event or full snapshot landed while this read was in flight.
      // Its state is newer than this response, so drop the response rather
      // than regress the visible indicator. The queued trailing read (or the
      // snapshot that superseded us) carries the newer state.
      if (revision !== circuitOwnershipsRevision) return;
      const { ownerships, changed } = reconcileOwnerships(get().circuitOwnerships, rows);
      // Nothing moved (the common case for a burst's trailing read) — keep the
      // same map reference so no subscriber re-renders.
      if (changed) set({ circuitOwnerships: ownerships });
    } catch {
      // A transient read failure preserves the last known ownership instead of
      // clearing a live indicator; the next event or fetch retries.
    }
  });

  // Conditional full refresh for event handlers (issue #1751). Skips
  // the `fetchAgentNodes` fan-out when the last successful snapshot is
  // still within `REFRESH_IF_STALE_MS` AND every scoped id is already
  // in the map — a duplicate `node-created` for a known row, or an
  // event burst on the heels of a snapshot. An unknown scoped id (a
  // genuinely new row) or an expired stamp always fetches. Delegates to
  // the coalesced `fetchAgentNodes`, so concurrent callers still share
  // one trailing refresh; the shallow reconciliation there keeps
  // per-node identity for unchanged rows (issue #1748 constraint).
  const refreshIfStale = async (nodeIds?: number[]): Promise<void> => {
    const fresh = Date.now() - lastFetchedAtMs < REFRESH_IF_STALE_MS;
    if (fresh && (!nodeIds || nodeIds.every((id) => get().nodesById[id] !== undefined))) {
      return;
    }
    await fetchAgentNodes();
  };

  return {
  nodesById: {},
  nodeIds: [],
  circuitOwnerships: {},
  semanticTurns: {},
  activeNodeId: null,
  loading: false,
  error: null,
  closingNodeIds: new Set(),
  stalledInputs: {},

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

  fetchAgentNodes,
  refreshIfStale,

  // Issue #1054 — typed dispatch surface for `agentNodeListeners.ts`.
  // One-liners that the listeners dispatch through; also exposed on the
  // public surface in case future code wants the same seam. Issue #1384 —
  // now updates the single entry in `nodesById` (preserving identity for
  // every other node), rather than remapping the whole array.
  patchAgentNode: (id, patch) => {
    if (patch.status === 'idle' && runtime.failedAutoSpawns.has(id)) return;
    if (patch.status === 'error' && get().nodesById[id]) runtime.failedAutoSpawns.add(id);
    runtime.nodeRevision += 1;
    // Issue #2325 — a delivered event is the newest observation of the
    // columns it carries, including one another window's `node-renamed`.
    // Hand those columns over: an in-flight rename must neither roll its
    // own failure back over the event nor adopt anything afterwards. The
    // full-snapshot path deliberately does NOT release, because a
    // snapshot can predate the mutation it would otherwise retire — the
    // mutation's own settlement reconciles its columns afterwards.
    optimisticLedger.release(id, Object.keys(patch) as AgentNodeField[]);
    if (patch.status && !['idle', 'spawning', 'error'].includes(patch.status)) {
      runtime.spawnRequests.delete(id);
      runtime.failedAutoSpawns.delete(id);
    }
    set((state) => {
      const current = state.nodesById[id];
      if (!current) return state;
      return {
        nodesById: { ...state.nodesById, [id]: { ...current, ...patch } },
      };
    });
  },
  patchCircuitOwnershipState: (runId, state) => {
    // Any recognised run-state event is newer than an in-flight satellite
    // read, so bump before applying: a read that resolves after this event
    // must not commit its older snapshot over it. Bump even when no row
    // matches — the event still describes newer state for that run.
    circuitOwnershipsRevision += 1;
    set((current) => {
      let changed = false;
      const circuitOwnerships = Object.fromEntries(
        Object.entries(current.circuitOwnerships).map(([nodeId, ownership]) => {
          if (ownership.run_id !== runId || ownership.state === state) return [nodeId, ownership];
          changed = true;
          return [nodeId, { ...ownership, state }];
        }),
      );
      return changed ? { circuitOwnerships } : current;
    });
  },
  refreshCircuitOwnerships,
  findAgentNode: (id) => get().nodesById[id],

  ...(() => {
    // Issue #1524 — attachment is a three-state machine, not a boolean.
    // The old `listenersAttached = true` was set BEFORE the await, so a
    // registration that failed part-way through left the store believing
    // it was wired: the Boot Error Panel's Retry short-circuited on the
    // flag and the store stayed permanently deaf to lifecycle events.
    //   idle      — nothing attached; the next call attaches.
    //   attaching — a registration is in flight; concurrent callers share
    //                that one attempt through `attachPromise`, so React
    //                StrictMode's double-mount cannot register an event
    //                twice.
    //   attached  — every listener registered; later calls are no-ops.
    //
    // `attachAgentNodeListeners` returns a cleanup handle for the complete
    // set. It is deliberately not held here: the store belongs to the
    // webview and outlives every component, so no caller has a teardown
    // point to pass it to, and an unconsumed handle is dead weight. The
    // rollback that *is* needed — undoing a partial registration so a
    // retry starts from a clean bus — is owned by the listener module,
    // which is why a failed attempt can safely return to `idle`.
    let attachment: 'idle' | 'attaching' | 'attached' = 'idle';
    let attachPromise: Promise<void> | null = null;

    // Issue #1530 - the transport stalled-input watch is a separate concern
    // from the lifecycle bus, so it gets its own one-way latch rather than
    // a slot in the attachment state machine: it cannot fail, has no
    // teardown, and must not be rolled back by (or block a retry of) a
    // failed lifecycle attach. Independent by design, so the two never
    // entangle.
    let stallWatchAttached = false;

    // Issue #1524 — hand the reset to the module-level test seam below, the
    // same shape as `setWorktreeCloseActionResolverForTests`. The state is a
    // `create()` closure, so it is per test *file*; without a reset a test
    // that attaches first would silently become another test's setup.
    resetAgentNodeListenerAttachment = () => {
      attachment = 'idle';
      attachPromise = null;
      stallWatchAttached = false;
    };

    return {
      // Issue #1530 - mirror the transport ordered retry buffer into the
      // store. This is the transport *subscribing* to a UI concern, so the
      // dependency arrow still points store -> transport; nothing in
      // `lib/` reaches back into a store.
      initTerminalInputWatch: () => {
        if (stallWatchAttached) return;
        stallWatchAttached = true;
        api.subscribeTerminalInputStall((nodeId, stall) => get().setStalledInput(nodeId, stall));
      },
      initAttentionListeners: () => {
        // Wire the input watch on the first attempt whatever the lifecycle
        // bus goes on to do, so a failed attach can be retried without
        // leaving a user typing into a terminal whose stall is never shown.
        get().initTerminalInputWatch();
        if (attachment === 'attached') return Promise.resolve();
        if (attachPromise) return attachPromise;

        attachment = 'attaching';
        const attempt = attachAgentNodeListeners({
          fetchAgentNodes: get().fetchAgentNodes,
          refreshIfStale: get().refreshIfStale,
          setActiveNode: get().setActiveNode,
          patchAgentNode: get().patchAgentNode,
          patchCircuitOwnershipState: get().patchCircuitOwnershipState,
          refreshCircuitOwnerships: get().refreshCircuitOwnerships,
          setSemanticTurn: get().setSemanticTurn,
          findAgentNode: get().findAgentNode,
          removeAgentNode: get().removeAgentNode,
        })
          .then(
            () => {
              // Only a complete registration counts as attached.
              attachment = 'attached';
            },
            (e) => {
              // `attachAgentNodeListeners` already rolled back the
              // partial set; drop to idle so the next call retries.
              attachment = 'idle';
              throw e;
            },
          )
          .finally(() => {
            if (attachPromise === attempt) attachPromise = null;
          });
        attachPromise = attempt;
        return attempt;
      },
    };
  })(),

  ...createCreationSlice(set, get, runtime),

  ...createCloseSlice(set, get, runtime, optimisticLedger),

  ...createMutationSlice(set, get, optimisticSurface, optimisticLedger),

  setActiveNode: (id) => { set({ activeNodeId: id }); },

  ...createLifecycleSlice(set, get, runtime),

  setSemanticTurn: (id, turn) => {
    set((state) => {
      const semanticTurns = { ...state.semanticTurns };
      if (turn) semanticTurns[id] = turn;
      else delete semanticTurns[id];
      return { semanticTurns };
    });
  },

  setStalledInput: (nodeId, stall) => {
    // The invariant lives here, not in the callers: each node owns its own
    // entry, so the transport, the terminal registry and any future entrypoint
    // cannot desynchronize a badge from the buffer that owns those bytes
    // (issue #1002's rule, applied to this state). Critically, a `null` removes
    // only `nodeId`'s entry — withdrawing one node's stall must never clear
    // another node's, which a single shared slot could not express.
    set((state) => {
      const current = state.stalledInputs[nodeId];
      if (stall === null) {
        if (current === undefined) return {};
        const next = { ...state.stalledInputs };
        delete next[nodeId];
        return { stalledInputs: next };
      }
      // The buffer re-notifies as the backlog grows, so this comparison is what
      // keeps an unchanged stall from re-rendering every node header.
      if (current && current.pendingBytes === stall.pendingBytes && current.attempts === stall.attempts) {
        return {};
      }
      return { stalledInputs: { ...state.stalledInputs, [nodeId]: stall } };
    });
  },
  };
});
