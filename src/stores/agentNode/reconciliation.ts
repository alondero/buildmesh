import type { AgentNode } from '../../types/generated/AgentNode';
import type { CircuitAgentOwnership } from '../../types/generated/CircuitAgentOwnership';

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
  lifecycle: true,
  worktree_path: true,
  project_scope: true,
  launch_configuration: true,
};
const AGENT_NODE_RECONCILE_FIELDS = Object.keys(
  AGENT_NODE_RECONCILE_SCHEMA,
) as ReadonlyArray<ReconciledKey>;

export function shallowEqualAgentNode(a: AgentNode, b: AgentNode): boolean {
  for (const k of AGENT_NODE_RECONCILE_FIELDS) {
    if (k === 'launch_configuration' || k === 'lifecycle') {
      if (JSON.stringify(a[k]) !== JSON.stringify(b[k])) return false;
      continue;
    }
    if (a[k] !== b[k]) return false;
  }
  return true;
}

// Same exhaustiveness contract as `AGENT_NODE_RECONCILE_SCHEMA` above. The
// ownership wire type has already gained fields once, and an unreconciled
// field would make a refresh keep a stale row's object identity (silently
// skipping the re-render that should have shown the change). Listing every
// key here makes `tsc` fail at the next `cargo test` regeneration instead.
type OwnershipReconciledKey = keyof CircuitAgentOwnership;
const CIRCUIT_OWNERSHIP_RECONCILE_SCHEMA: Record<OwnershipReconciledKey, true> = {
  node_id: true,
  run_id: true,
  circuit_id: true,
  circuit_name: true,
  state: true,
  parent_node_id: true,
};
const CIRCUIT_OWNERSHIP_RECONCILE_FIELDS = Object.keys(
  CIRCUIT_OWNERSHIP_RECONCILE_SCHEMA,
) as ReadonlyArray<OwnershipReconciledKey>;

function shallowEqualOwnership(a: CircuitAgentOwnership, b: CircuitAgentOwnership): boolean {
  for (const k of CIRCUIT_OWNERSHIP_RECONCILE_FIELDS) {
    if (a[k] !== b[k]) return false;
  }
  return true;
}

/// Fold a fetched ownership list into the existing map, keeping the previous
/// object identity for rows that did not change. Shared by both read paths
/// (`fetchAgentNodes` and `refreshCircuitOwnerships`) so a full refetch does
/// not allocate fresh ownership objects for every node and defeat per-id
/// selector caching. `changed` reports whether anything actually moved, which
/// lets a no-op satellite refresh skip its `set` entirely.
export function reconcileOwnerships(
  current: Readonly<Record<number, CircuitAgentOwnership>>,
  rows: readonly CircuitAgentOwnership[],
): { ownerships: Record<number, CircuitAgentOwnership>; changed: boolean } {
  const ownerships: Record<number, CircuitAgentOwnership> = {};
  let changed = Object.keys(current).length !== rows.length;
  for (const ownership of rows) {
    const prior = current[ownership.node_id];
    const unchanged = prior !== undefined && shallowEqualOwnership(prior, ownership);
    ownerships[ownership.node_id] = unchanged ? prior : ownership;
    if (!unchanged) changed = true;
  }
  return { ownerships, changed };
}
