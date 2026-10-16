/**
 * Optimistic-with-rollback helper and field-ownership ledger for
 * `agentNodeStore` (issue #1054; ownership policy issue #2325).
 *
 * Three call sites in `agentNodeStore.ts` — `renameAgentNode`,
 * `setNodePinned`, `toggleNodePinned` — used to hand-roll the same
 * shape: patch the local state immediately, await the mutation, then
 * adopt the backend's row (pin actions) or wait for a separate
 * `node-renamed` event (rename). On rejection they all restored the
 * patched columns and wrote `state.error`.
 *
 * Authority policy (issue #2325)
 * ------------------------------
 * Three rules. The first two live entirely in this module; the third
 * needs the store to hand over the columns a delivered event owns:
 *
 *   1. **Events own their observations.** A delivered lifecycle or
 *      `node-renamed` event is the newest write for the columns it
 *      carries, so the store calls `release()` for exactly those keys.
 *   2. **Mutations own their fields.** A mutation reconciles only the
 *      columns in its `optimisticPatch`. A pin completion reads
 *      `is_pinned` off the backend row; it never adopts the row's
 *      `status`, `name`, or session columns, which may be older than
 *      observations that landed while the IPC was in flight.
 *   3. **Obsolete operations cannot commit or roll back.** Each
 *      operation claims its fields in the ledger; the newest claim
 *      owns them. A settled operation retires itself and everything it
 *      superseded, so a stale success adopts nothing and a stale
 *      failure restores nothing.
 *
 * A failure restores the column's last *acknowledged* value, not the
 * value this operation captured when it started. Capturing a prior per
 * operation is what made overlapping renames wrong: the newer rename's
 * rollback restored the older rename's still-uncommitted name, and two
 * failures left it standing. When an older operation is still in flight
 * the column falls back to that operation's value, and that operation
 * reconciles — or, if it also fails, restores the acknowledged value —
 * when it settles.
 *
 * Not for component-local toggles — see `optimisticToggle.ts` for the
 * AppSettings pattern (component-owned setters, not a Zustand store).
 */
import { formatError } from './errorUtils';
import type { AgentNode } from '../types/generated/AgentNode';

/** A column of `agent_nodes`. Ownership is tracked per column. */
export type AgentNodeField = keyof AgentNode;

/**
 * Narrow surface the helper writes through. Issue #1384 — the surface
 * is per-node (the store holds a normalized `nodesById` map), so
 * patches never disturb other entries and other entries' identity is
 * preserved across the optimistic window. The helper writes ONE node
 * at a time via `setAgentNode(id, updater)`; rollback uses the same
 * updater shape so it composes cleanly with the optimistic patch.
 */
export interface OptimisticSurface {
  /** Read the current row for `nodeId`. Used to capture `prior`
   *  (the rollback reference). Returns `undefined` if the node isn't
   *  loaded — the helper treats that as a thrown precondition. */
  getAgentNode: (nodeId: number) => AgentNode | undefined;
  /** Replace a single node in the store via a functional updater.
   *  Functional form only — matches Zustand's `set(updater)` and
   *  avoids stale-closure bugs. The store passes a function so we
   *  never re-open a full-replace path the helper never needs (issue
   *  #1054 review). */
  setAgentNode: (nodeId: number, next: (prev: AgentNode) => AgentNode) => void;
  /** Write `state.error` on the rejection path. */
  setError: (error: string | null) => void;
}

/** One in-flight optimistic operation and the fields it claimed. */
export interface OptimisticOperation {
  readonly nodeId: number;
  /** The optimistic values this operation claimed, one per owned field. */
  readonly values: Partial<AgentNode>;
}

export interface OptimisticFieldLedger {
  /** Take ownership of `patch`'s fields for a new operation. The first
   *  claim on a field records that field's currently acknowledged value,
   *  read from `prior` — the row is the only source of it. */
  begin: (nodeId: number, prior: AgentNode, patch: Partial<AgentNode>) => OptimisticOperation;
  /** Settle after the backend acknowledged the mutation. `acknowledged`
   *  is the authoritative row for the operation's own fields; a field it
   *  omits falls back to the operation's optimistic value. Returns the
   *  patch the caller should write (empty when a newer operation still
   *  owns the fields). */
  commit: (op: OptimisticOperation, acknowledged: Partial<AgentNode>) => Partial<AgentNode>;
  /** Settle after the mutation failed. Returns the patch the caller
   *  should write — the last acknowledged value, or the value a still
   *  pending older operation is showing. */
  rollback: (op: OptimisticOperation) => Partial<AgentNode>;
  /** Hand `fields` to a newer authoritative observation (a delivered
   *  event). Every in-flight mutation of those fields becomes obsolete
   *  and can no longer commit or roll back. */
  release: (nodeId: number, fields: readonly AgentNodeField[]) => void;
  /** Drop all ledger state for a node that left the store. */
  forget: (nodeId: number) => void;
}

/** In-flight writes to one column, oldest first. The last entry owns it. */
interface FieldOwnership {
  /** Last value known to be committed. Captured from the row when the
   *  field is first claimed, then advanced by every acknowledgement. */
  durable: unknown;
  pending: Array<{ op: OptimisticOperation; value: unknown }>;
}

export interface OptimisticLedgerOptions {
  /** Called whenever an operation starts or settles for a node, whether
   *  or not it changes a visible value. The store uses this to fence
   *  full reads: a `list_agent_nodes` snapshot that began before the
   *  operation predates it and must not be painted over its fields. */
  onFieldWrite?: (nodeId: number) => void;
}

/**
 * Build the per-node field-ownership ledger. One instance per store —
 * the state is deliberately not module-level, so a fresh store (or a
 * test) starts with no claims.
 */
export function createOptimisticFieldLedger(
  options: OptimisticLedgerOptions = {},
): OptimisticFieldLedger {
  const nodes = new Map<number, Map<AgentNodeField, FieldOwnership>>();
  const prune = (nodeId: number, fields: Map<AgentNodeField, FieldOwnership>) => {
    if (fields.size === 0) nodes.delete(nodeId);
  };

  return {
    begin(nodeId, prior, patch) {
      const op: OptimisticOperation = { nodeId, values: { ...patch } };
      let fields = nodes.get(nodeId);
      if (!fields) {
        fields = new Map();
        nodes.set(nodeId, fields);
      }
      for (const field of Object.keys(patch) as AgentNodeField[]) {
        let ownership = fields.get(field);
        if (!ownership) {
          ownership = { durable: prior[field], pending: [] };
          fields.set(field, ownership);
        }
        ownership.pending.push({ op, value: patch[field] });
      }
      options.onFieldWrite?.(nodeId);
      return op;
    },

    commit(op, acknowledged) {
      // Values are `unknown` in the ledger (a column may be a string, a
      // number, a nested generated type), and TypeScript cannot assign
      // through a union-keyed index signature, so the patch is built
      // untyped and cast once on the way out. Every key written here is a
      // column of `op.values`, so the cast is sound by construction.
      const settled: Record<string, unknown> = {};
      const fields = nodes.get(op.nodeId);
      if (!fields) return settled;
      for (const field of Object.keys(op.values) as AgentNodeField[]) {
        const ownership = fields.get(field);
        const index = ownership?.pending.findIndex(w => w.op === op) ?? -1;
        if (!ownership || index === -1) continue;
        // An acknowledgement is the newest *committed* value even when a
        // newer operation is showing something else, so the durable
        // baseline advances whether or not this operation still owns the
        // column.
        ownership.durable = field in acknowledged ? acknowledged[field] : op.values[field];
        const ownsColumn = index === ownership.pending.length - 1;
        // Retire this operation and every older one on the column: a
        // superseded operation must not commit after the write that
        // replaced it, even if it settles later.
        ownership.pending.splice(0, index + 1);
        if (ownership.pending.length === 0) fields.delete(field);
        if (ownsColumn) settled[field] = ownership.durable;
      }
      prune(op.nodeId, fields);
      options.onFieldWrite?.(op.nodeId);
      return settled as Partial<AgentNode>;
    },

    rollback(op) {
      const settled: Record<string, unknown> = {};
      const fields = nodes.get(op.nodeId);
      if (!fields) return settled;
      for (const field of Object.keys(op.values) as AgentNodeField[]) {
        const ownership = fields.get(field);
        const index = ownership?.pending.findIndex(w => w.op === op) ?? -1;
        if (!ownership || index === -1) continue;
        const ownsColumn = index === ownership.pending.length - 1;
        ownership.pending.splice(index, 1);
        // A newer operation still owns the column — its value is what the
        // row shows, so this failure has nothing to restore.
        if (!ownsColumn) continue;
        // An older operation is still in flight and is showing its own
        // value; failing it will restore the durable baseline from there,
        // so never paint a value that is no longer committed.
        settled[field] = ownership.pending.length
          ? ownership.pending[ownership.pending.length - 1].value
          : ownership.durable;
        if (ownership.pending.length === 0) fields.delete(field);
      }
      prune(op.nodeId, fields);
      options.onFieldWrite?.(op.nodeId);
      return settled as Partial<AgentNode>;
    },

    release(nodeId, released) {
      const fields = nodes.get(nodeId);
      if (!fields) return;
      for (const field of released) fields.delete(field);
      prune(nodeId, fields);
    },

    forget(nodeId) {
      nodes.delete(nodeId);
    },
  };
}

export interface WithOptimisticArgs<TPatch extends Partial<AgentNode>, TResult> {
  surface: OptimisticSurface;
  /** Field-ownership ledger shared with the store, so an event that
   *  supersedes these fields can retire the claim (issue #2325). */
  ledger: OptimisticFieldLedger;
  /** Node id to update. The helper rejects with a clear error if the
   *  node isn't loaded — matches the explicit `prior` precondition in
   *  the three pre-refactor inline patterns. */
  nodeId: number;
  /** Columns to patch optimistically. Also the reconciliation scope — a
   *  success writes only these keys (from the mutation's result), and a
   *  rejection restores only these keys. */
  optimisticPatch: TPatch;
  /** The backend mutation. The promise's resolved value is forwarded
   *  to `adoptResult`; its rejection drives the rollback path. */
  mutation: () => Promise<TResult>;
  /** Optional — read the mutation's resolved value as the authority for
   *  `optimisticPatch`'s columns. Used by the pin actions, whose backend
   *  row carries the committed `is_pinned`. Only the claimed columns are
   *  read: the row's other columns may be older than observations that
   *  landed while the IPC was in flight (issue #2325). Omit for
   *  `renameAgentNode`, where a separate `node-renamed` event is the
   *  cross-window source of truth and the optimistic value is already
   *  the committed one. */
  adoptResult?: (result: TResult) => AgentNode | undefined;
}

/**
 * Write the subset of `patch` whose values differ from the current row.
 * Skipping an equal write keeps the row's object identity stable, so a
 * pin that the backend confirmed with the value already on screen does
 * not re-render per-id subscribers for nothing.
 */
function writeFields(
  surface: OptimisticSurface,
  nodeId: number,
  patch: Partial<AgentNode>,
): void {
  const current = surface.getAgentNode(nodeId);
  // The node left the store while the mutation was in flight. Writing it
  // back would resurrect a deleted row, so the operation simply stops.
  if (!current) return;
  // Same union-keyed index limitation as the ledger: collect into an
  // untyped record and cast once. Every key comes from a patch the caller
  // built from `AgentNode` columns, so the cast is sound.
  const changed: Record<string, unknown> = {};
  let dirty = false;
  for (const [key, value] of Object.entries(patch)) {
    if (current[key as AgentNodeField] !== value) {
      changed[key] = value;
      dirty = true;
    }
  }
  if (!dirty) return;
  surface.setAgentNode(nodeId, prev => ({ ...prev, ...changed as Partial<AgentNode> }));
}

/**
 * Apply `optimisticPatch` to the local node immediately, run
 * `mutation`, then reconcile the patched columns from the ledger's
 * ownership decision: a success writes the acknowledged value for the
 * columns this operation still owns, and a rejection restores the last
 * acknowledged value. Both paths are a no-op once a newer operation or
 * a newer observation has taken the columns over. On rejection the
 * helper also writes `surface.setError(formatError(e))` and re-throws.
 */
export async function withOptimistic<TPatch extends Partial<AgentNode>, TResult>(
  args: WithOptimisticArgs<TPatch, TResult>,
): Promise<TResult> {
  const prior = args.surface.getAgentNode(args.nodeId);
  if (!prior) {
    throw new Error(`withOptimistic: node ${args.nodeId} is not loaded`);
  }
  const op = args.ledger.begin(args.nodeId, prior, args.optimisticPatch);
  writeFields(args.surface, args.nodeId, args.optimisticPatch);

  try {
    const result = await args.mutation();
    // Only the claimed columns are adopted. The backend row is
    // authoritative for the pin column it changed; it is not evidence
    // that the lifecycle, session, or rename columns it happened to
    // carry are still current.
    const acknowledged = args.adoptResult?.(result) ?? {};
    writeFields(args.surface, args.nodeId, args.ledger.commit(op, acknowledged));
    return result;
  } catch (e) {
    writeFields(args.surface, args.nodeId, args.ledger.rollback(op));
    args.surface.setError(formatError(e));
    throw e;
  }
}