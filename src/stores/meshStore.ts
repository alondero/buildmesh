import { formatError } from '../lib/errorUtils';
import { create } from 'zustand';
import * as api from '../lib/tauri';
import { addToast } from './toastStore';
import { SCOPE_NOTICE_PROVIDER, meshDeletedScopeNotice } from '../lib/scopeNotices';
import type { Mesh } from '../types/generated/Mesh';
// Deletion loads resource cleanup and navigation lazily; neither is needed
// to initialize the mesh data store.
type DisposeTerminalFn = (nodeId: number) => void;
type AgentNodeStoreModule = typeof import('./agentNodeStore');

// `Mesh` is generated from the Rust `models::Mesh` struct (issue #359).
// Re-exported here so the many `import { Mesh } from '../stores/meshStore'`
// call sites keep working. The generated type is the full 14-field wire shape,
// not the 6-field subset this store used to hand-declare.
export type { Mesh };

/**
 * What one snapshot read did, reported to the caller that started it.
 */
type SnapshotOutcome =
  /** Applied: this read is the newest state the store holds. */
  | { status: 'applied' }
  /**
   * Obsolete: a newer read was already accepted, or a mutation landed while this
   * one was in flight. Its response was dropped, so `error`/`loading` were left
   * to the newer owner. `followUp` is the trailing authoritative read that will
   * converge the collection, or `null` when convergence belongs to the operation
   * that superseded this read.
   */
  | { status: 'superseded'; followUp: Promise<SnapshotOutcome> | null }
  /** Failed while still authoritative, so the caller owns the rejection. */
  | { status: 'failed'; error: unknown };

/**
 * Build both collection views from one pass over the incoming rows, keeping the
 * previous object reference for every row whose fields compare equal. Callers
 * that select one Mesh (`s.meshesById.get(id)`) then skip a render when only
 * other Meshes changed, and `meshes`/`meshesById` can never disagree about a
 * row because they share the same reconciled objects.
 */
function reconcileMeshes(
  previous: Map<number, Mesh>,
  incoming: Mesh[]
): { meshes: Mesh[]; meshesById: Map<number, Mesh> } {
  const meshes: Mesh[] = [];
  const meshesById = new Map<number, Mesh>();
  for (const row of incoming) {
    const old = previous.get(row.id);
    const reconciled = old && shallowEqualMesh(old, row) ? old : row;
    meshes.push(reconciled);
    meshesById.set(row.id, reconciled);
  }
  return { meshes, meshesById };
}

/** Every `Mesh` field is a primitive, so a key-wise compare is a shallow compare. */
function shallowEqualMesh(a: Mesh, b: Mesh): boolean {
  const keys = Object.keys(a) as (keyof Mesh)[];
  if (keys.length !== Object.keys(b).length) return false;
  return keys.every((key) => a[key] === b[key]);
}

interface MeshState {
  meshes: Mesh[];
  meshesById: Map<number, Mesh>;
  selectedMeshId: number | null;
  loading: boolean;
  error: string | null;
  /**
   * Record a snapshot failure in `state.error` and resolve either way. Callers
   * that need the outcome (boot readiness, the capacity editor) use
   * `refreshMeshes`.
   */
  fetchMeshes: () => Promise<void>;
  /**
   * Refresh the mesh snapshot and reject on failure for callers that must
   * confirm it succeeded. A read superseded by a newer write resolves once the
   * trailing refresh it started lands: the superseded read itself has no failure
   * to report, and its snapshot was never applied.
   */
  refreshMeshes: () => Promise<void>;
  addMesh: () => Promise<void>;
  createMesh: (name: string, path: string, color?: string | null) => Promise<Mesh | null>;
  // Clone a GitHub repo into `<parentDir>/<repo>` and register it as a mesh.
  // Returns the created mesh or the backend's error text: clone failures are
  // user-actionable (repo not found, auth, destination already exists), so the
  // caller needs the real message rather than the generic `Mesh | null`
  // fallback that buries it in `state.error`.
  cloneMesh: (
    url: string,
    parentDir: string,
    color?: string | null
  ) => Promise<{ mesh: Mesh } | { error: string }>;
  // Issue #1247 — returns `true` on successful delete + cleanup so callers
  // can gate their own follow-up (Probe close, navigation, toast) on the
  // real outcome instead of catching an error `deleteMesh` never throws.
  // Errors continue to be surfaced via `state.error` to match the rest of
  // the meshStore surface.
  deleteMesh: (id: number) => Promise<boolean>;
  updateMeshColor: (id: number, color: string | null) => Promise<void>;
  selectMesh: (id: number | null) => void;
  updateMeshLayout: (id: number, layout: 'grid' | 'single') => Promise<void>;
  reorderMeshes: (meshId: number, newPosition: number) => Promise<void>;
  updateMeshName: (id: number, name: string) => Promise<void>;
  getDefaultProvider: (meshId: number) => Promise<string>;
}

export const useMeshStore = create<MeshState>((set, get) => {
  // ---- Collection authority ledger (issue #2326) ---------------------------
  // A snapshot read may replace the collection only while it is still the
  // store's authority. Three things can take that authority away while it is in
  // flight, and all three are observed here rather than in the caller:
  //
  //  * `nextRequestId` / `acceptedRequestId` — request identity. Two overlapping
  //    callers each get their own read; whichever response lands second may only
  //    commit if it is newer than the one already accepted. Without it the older
  //    snapshot simply overwrote the newer one.
  //  * `collectionRevision` — mutation authority. Every committed local write
  //    (create/add/clone, delete commit, rename/color/layout, the optimistic
  //    reorder) advances it, so a read that observed an older revision is
  //    describing a database state the user has already moved past.
  //  * `pendingReorders` — an optimistic reorder is deliberately ahead of the
  //    persisted order until it is acknowledged, so no snapshot may replace it,
  //    not even one started after the optimistic write.
  //
  // `readsInFlight` owns `loading`: a read that is dropped must not mark a
  // newer outstanding read as finished. `trailingRead` is the single refresh that
  // converges a discarded read whose committed write no applied snapshot has
  // seen yet, shared by every discard of the same newer state and released the
  // moment it settles; `MAX_TRAILING_REARMS` bounds how many replacements one
  // discard chain may spend, so convergence never becomes a retry loop.
  const MAX_TRAILING_REARMS = 1;
  let nextRequestId = 1;
  let acceptedRequestId = 0;
  // The revision the newest applied snapshot observed. Equal to
  // `collectionRevision` means every committed mutation is already on screen.
  let acceptedRevision = -1;
  let collectionRevision = 0;
  let pendingReorders = 0;
  let readsDroppedDuringReorder = 0;
  let readsInFlight = 0;
  let trailingRead: Promise<SnapshotOutcome> | null = null;
  let trailingOwnerId = 0;

  /** A response may commit only while nothing newer has taken authority. */
  function stillAuthoritative(requestId: number, observedRevision: number): boolean {
    return requestId > acceptedRequestId
      && observedRevision === collectionRevision
      && pendingReorders === 0;
  }

  /**
   * One trailing authoritative read after an obsolete response was dropped.
   * `selfId` is the discarded read, or `null` when the caller is not a read (a
   * settled reorder). `trailingDepth` counts the replacements this chain has
   * already spent: a dropped read converges through one, and a second write
   * landing while that one is in flight may replace it once more. Past that the
   * chain stops, so a continuous stream of writes cannot turn convergence into a
   * refresh loop.
   */
  function scheduleTrailingRead(selfId: number | null, trailingDepth: number): Promise<SnapshotOutcome> | null {
    // While a reorder is outstanding the local order is ahead of the persisted
    // one, and the trailing read would be dropped exactly like the read that
    // asked for it. The reorder's acknowledgement converges instead.
    if (pendingReorders > 0) return null;
    if (trailingDepth > MAX_TRAILING_REARMS) return null;
    // Another read's trailing refresh is already outstanding and will observe
    // the same newer state, so share it rather than open a competing snapshot.
    if (trailingRead && trailingOwnerId !== selfId) return trailingRead;
    const ownerId = nextRequestId++;
    trailingOwnerId = ownerId;
    trailingRead = readSnapshot(ownerId, trailingDepth + 1);
    return trailingRead;
  }

  /** Keep the newer state, and record how this read lost authority. */
  function discardRead(requestId: number, trailingDepth: number): SnapshotOutcome {
    if (pendingReorders > 0) {
      readsDroppedDuringReorder += 1;
      return { status: 'superseded', followUp: null };
    }
    // A trailing read exists to pick up a committed write that no applied
    // snapshot reflects yet. When the newest applied read already observed the
    // current revision, convergence has happened — the read that beat this one
    // was newer and saw everything this one would have — so a refresh here
    // would be pure IPC and loading churn.
    if (acceptedRevision === collectionRevision) {
      return { status: 'superseded', followUp: null };
    }
    return { status: 'superseded', followUp: scheduleTrailingRead(requestId, trailingDepth) };
  }

  /**
   * One snapshot read. Never throws: the reject/non-throwing split lives in the
   * two public actions, which report this read's own outcome.
   */
  async function readSnapshot(requestId: number, trailingDepth = 0): Promise<SnapshotOutcome> {
    const observedRevision = collectionRevision;
    readsInFlight += 1;
    set({ loading: true, error: null });
    try {
      const incoming = await api.listMeshes();
      if (!stillAuthoritative(requestId, observedRevision)) {
        return discardRead(requestId, trailingDepth);
      }
      acceptedRequestId = requestId;
      acceptedRevision = observedRevision;
      // Clear `error` here as well as at the start of the read: an older failure
      // that was still authoritative when it landed (its newer rival had not
      // answered yet) must not outlive the snapshot that supersedes it.
      set({ ...reconcileMeshes(get().meshesById, incoming), error: null });
      return { status: 'applied' };
    } catch (e) {
      // An obsolete failure must not replace a newer success with an error.
      if (!stillAuthoritative(requestId, observedRevision)) {
        return discardRead(requestId, trailingDepth);
      }
      set({ error: formatError(e) });
      return { status: 'failed', error: e };
    } finally {
      readsInFlight -= 1;
      // Release the trailing slot in THIS tick, not in a later microtask: a
      // discard that lands right after this read settled must start its own
      // refresh instead of joining an already-settled one and skipping
      // convergence entirely.
      if (trailingOwnerId === requestId) {
        trailingRead = null;
        trailingOwnerId = 0;
      }
      // `loading` belongs to whatever read is still outstanding, so a dropped
      // response cannot mark a newer read — or the trailing refresh it started
      // — as finished.
      if (readsInFlight === 0) set({ loading: false });
    }
  }

  /** Advance the ledger when a committed write lands on a row this store holds. */
  function commitRowMutation(id: number): void {
    if (get().meshesById.has(id)) collectionRevision += 1;
  }

  return ({
  meshes: [],
  meshesById: new Map(),
  selectedMeshId: null,
  loading: false,
  error: null,

  fetchMeshes: async () => {
    await readSnapshot(nextRequestId++);
  },

  refreshMeshes: async () => {
    // Follow this caller's read through the trailing refresh a superseded read
    // starts, so it settles on an applied snapshot rather than on a discarded
    // one. An obsolete read has no failure of its own to report: the trailing
    // read, or the newer success that replaced it, is what this resolves on.
    let outcome = await readSnapshot(nextRequestId++);
    while (outcome.status === 'superseded' && outcome.followUp) {
      outcome = await outcome.followUp;
    }
    if (outcome.status === 'failed') throw outcome.error;
  },

  addMesh: async () => {
    try {
      const mesh = await api.addMesh();
      collectionRevision += 1;
      set((state) => ({
        meshes: [...state.meshes, mesh],
        meshesById: new Map([...state.meshesById, [mesh.id, mesh]])
      }));
    } catch (e) {
      set({ error: formatError(e) });
    }
  },

  createMesh: async (name, path, color) => {
    try {
      const mesh = await api.createMesh(name, path, color);
      collectionRevision += 1;
      set((state) => ({
        meshes: [...state.meshes, mesh],
        meshesById: new Map([...state.meshesById, [mesh.id, mesh]]),
      }));
      return mesh;
    } catch (e) {
      set({ error: formatError(e) });
      return null;
    }
  },

  cloneMesh: async (url, parentDir, color) => {
    try {
      const mesh = await api.cloneMeshRepo(url, parentDir, color);
      collectionRevision += 1;
      set((state) => ({
        meshes: [...state.meshes, mesh],
        meshesById: new Map([...state.meshesById, [mesh.id, mesh]]),
      }));
      return { mesh };
    } catch (e) {
      const error = formatError(e);
      set({ error });
      return { error };
    }
  },

  deleteMesh: async (id): Promise<boolean> => {
    // Issue #1247 — capture doomed node ids BEFORE the backend delete
    // commits. Pre-#1247 the function only refetched meshes, which left:
    //   * ghost `agentNodes` rows still clickable in the grid + sidebar —
    //     clicking × fired `delete_agent_node` on a vanished row and
    //     surfaced a spurious error toast;
    //   * xterm instances + 'agent-output' listeners alive in
    //     `TerminalRegistry.instances` (the terminal-persistence rule is
    //     satisfied because the node IS deleted — `disposeTerminal` is
    //     the same dispose-on-delete site `agentNodeStore.deleteAgentNode`
    //     uses on its success path);
    //   * `selectedMeshId` / `activeNodeId` dangling into the deleted
    //     mesh — the Probe rendered against
    //     `meshesById.get(danglingId) === undefined`.
    //
    // Lazy-import the agent-node store to avoid the
    // meshStore → agentNodeStore → Terminal → uiStore → meshStore
    // ESM cycle (see the import comment at the top of this file).
    // The capture is the FIRST thing the method does so a doomed-id
    // capture failure (highly unlikely, but the IPC below is what
    // really fails) can't strand the dispose loop without its target
    // list. The `await` is the same one a static import would have
    // hidden — `deleteMesh` is already async.
    const agentNodeModule = await import('./agentNodeStore') as AgentNodeStoreModule;
    const { useAgentNodeStore } = agentNodeModule;
    const agentStoreBefore = useAgentNodeStore.getState();
    // Issue #1384 — iterate `nodeIds` and dereference through `nodesById`
    // rather than reading the (now-removed) `agentNodes` array. We only
    // care which ids belong to the doomed mesh, so we collect the ids
    // directly without rebuilding the node objects.
    const doomedNodeIds: number[] = [];
    for (const nid of agentStoreBefore.nodeIds) {
      const n = agentStoreBefore.nodesById[nid];
      if (n && n.mesh_id === id) doomedNodeIds.push(nid);
    }
    // #2076 — the scope notice names the Mesh, so read the name BEFORE the
    // post-delete refetch drops it from `meshesById`. The id fallback keeps
    // the copy honest for a Mesh this store never had loaded.
    const doomedMeshName = useMeshStore.getState().meshesById.get(id)?.name ?? `Mesh #${id}`;

    try {
      await api.deleteMesh(id);
    } catch (e) {
      set({ error: formatError(e) });
      return false;
    }

    // The backend committed the removal, and this store applies it through the
    // refetch below. Advance the ledger first so a snapshot taken before the
    // delete is dropped instead of putting the row back into both views.
    collectionRevision += 1;

    // Backend committed — fan out the post-delete cleanup. Refetch
    // BEFORE disposing so the agent-node store's invariant ("never
    // dispose unless the row is deleted", issue #647) holds when we
    // call `disposeTerminal` below: the refetch just observed the row
    // is gone, and we know for sure the doomed ids are now DB-dead.
    await useMeshStore.getState().fetchMeshes();
    await useAgentNodeStore.getState().fetchAgentNodes();

    // One dispose per doomed node. `disposeTerminal` is a no-op for
    // node ids that were never mounted (the registry's `instances.get`
    // returns `undefined`), so a doomed id that never opened a
    // terminal still gets a free, idempotent call — matches the
    // deleteAgentNode success path's "always dispose, the registry
    // absorbs the no-op" discipline.
    //
    // Lazy-imported to break the cycle (see the import comment).
    if (doomedNodeIds.length > 0) {
      const terminalModule = await import('../components/Terminal/Terminal') as {
        disposeTerminal: DisposeTerminalFn;
      };
      for (const nodeId of doomedNodeIds) {
        terminalModule.disposeTerminal(nodeId);
      }
    }

    // Clear dangling selection slots. Re-read AFTER the refetches so
    // a mesh-switch race (user picks a different mesh mid-await) does
    // NOT clobber a fresh `selectedMeshId` — only null the slot if it
    // STILL points into the deleted mesh after the cleanup. Same race
    // guard for `activeNodeId`: if the user clicked a different node
    // mid-await, that fresh active id is in a different mesh and we
    // leave it alone.
    const { useUIStore } = await import('./uiStore');
    const meshAfter = useMeshStore.getState();
    if (meshAfter.selectedMeshId === id) {
      useUIStore.getState().setViewMode('all');
      // #2076 — deleting the Mesh the user is looking at is a scope change
      // nobody asked for: the transition above moves the canvas to All Nodes. Announce it under the name of
      // what happened. `Mesh “X” was deleted — the canvas moved to All
      // Nodes.` Fire-and-forget through the shared toast stack, after the
      // selection write, so it describes a change that already happened and
      // cannot gate or undo the delete.
      addToast(SCOPE_NOTICE_PROVIDER, meshDeletedScopeNotice(doomedMeshName), 'info');
    }
    const agentAfter = useAgentNodeStore.getState();
    if (agentAfter.activeNodeId !== null && doomedNodeIds.includes(agentAfter.activeNodeId)) {
      agentAfter.setActiveNode(null);
    }

    return true;
  },

  selectMesh: (id) => set({ selectedMeshId: id }),

  updateMeshLayout: async (id, layout) => {
    try {
      await api.updateMeshLayout(id, layout);
      commitRowMutation(id);
      set((state) => {
        const existing = state.meshesById.get(id);
        if (!existing) return state;
        const updated = { ...existing, layout };
        return {
          meshes: state.meshes.map((p) => (p.id === id ? updated : p)),
          meshesById: new Map([...state.meshesById, [id, updated]])
        };
      });
    } catch (e) {
      set({ error: formatError(e) });
    }
  },

  reorderMeshes: async (meshId, newPosition) => {
    let updatedMeshes: Mesh[] | undefined;
    set((state) => {
      const meshes = [...state.meshes];
      const draggedIdx = meshes.findIndex((p) => p.id === meshId);
      if (draggedIdx === -1) return state;
      const [dragged] = meshes.splice(draggedIdx, 1);
      meshes.splice(newPosition, 0, dragged);
      updatedMeshes = meshes.map((p, idx) => ({ ...p, position: idx }));
      const meshesById = new Map(updatedMeshes.map((p) => [p.id, p]));
      return { meshes: updatedMeshes, meshesById };
    });
    // A Mesh this store never loaded moves nothing, so there is nothing to
    // persist. Reading `updatedMeshes.map` here instead threw a TypeError that
    // the catch below reported as a save error for a no-op drag.
    if (!updatedMeshes) return;

    // The optimistic order is now ahead of the persisted one, so it owns the
    // collection until the write is acknowledged: a read resolving in this
    // window must not undo a drag the user can see.
    collectionRevision += 1;
    pendingReorders += 1;
    let failed = false;
    try {
      // Send ALL meshes' positions so DB stays in sync with optimistic update
      const currentMeshes = updatedMeshes;
      const updates = currentMeshes.map((p) => [p.id, p.position] as [number, number]);
      await api.updateMeshPositions(updates);
    } catch (e) {
      set({ error: formatError(e) });
      failed = true;
    } finally {
      pendingReorders -= 1;
    }
    if (failed) {
      // The recovery read starts outside the reorder window, so it is
      // authoritative again and converges the order on its own.
      readsDroppedDuringReorder = 0;
      await useMeshStore.getState().fetchMeshes();
      return;
    }
    // Reads dropped during the window (a deletion's refetch, a capacity save)
    // never got a trailing refresh. The acknowledged order is authoritative by
    // itself, but whatever those reads still owed the collection is not.
    if (pendingReorders === 0 && readsDroppedDuringReorder > 0) {
      readsDroppedDuringReorder = 0;
      void scheduleTrailingRead(null, 0);
    }
  },

  // Issue #2024 rank 8 — a rejected rename must reject here too. Absorbing
  // the failure resolved the promise, so `ProjectSettingsTab`'s
  // `wrappedSave` ran its success branch and the tab reported "Saved" for a
  // write the backend never accepted, while the sidebar (which only updates
  // on success) kept the old name. Rethrowing after recording the error is
  // what lets the caller show the real rejection; the store's `error` field
  // is still set, so the destination-level banner path is unchanged.
  updateMeshName: async (id, name) => {
    try {
      await api.updateMeshName(id, name);
    } catch (e) {
      set({ error: formatError(e) });
      throw e;
    }
    commitRowMutation(id);
    set((state) => {
      const existing = state.meshesById.get(id);
      if (!existing) return state;
      const updated = { ...existing, name };
      return {
        meshes: state.meshes.map((m) => (m.id === id ? updated : m)),
        meshesById: new Map([...state.meshesById, [id, updated]])
      };
    });
  },

  updateMeshColor: async (id, color) => {
    try {
      await api.updateMeshColor(id, color);
      commitRowMutation(id);
      set((state) => {
        const existing = state.meshesById.get(id);
        if (!existing) return state;
        const updated = { ...existing, color };
        return {
          meshes: state.meshes.map((m) => (m.id === id ? updated : m)),
          meshesById: new Map([...state.meshesById, [id, updated]]),
        };
      });
    } catch (e) {
      set({ error: formatError(e) });
    }
  },

  getDefaultProvider: async (meshId) => {
    try {
      return await api.getDefaultProvider(meshId);
    } catch (e) {
      set({ error: formatError(e) });
      // Post-#538 the native Claude harness profile id is `'claude'`
      // (the unified-claude-harness change renamed the bare `'anthropic'`
      // provider id to the new harness-profile form). The backend's
      // `get_default_provider` IPC returns the post-#538 form, so the
      // fallback should mirror it for symmetry.
      return 'claude';
    }
  },
  });
});
