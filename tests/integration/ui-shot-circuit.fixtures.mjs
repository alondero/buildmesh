// The circuit screenshot exercises queue controls and the expanded run's
// wait/capacity/configuration/recovery history (#1909), not provider
// discovery. Leave the shared boot fixture untouched and remove spawn-menu
// data only for this focused smoke route.
export default {
  list_providers: [],
  // One entry per wait/capacity/configuration/recovery kind, rendered by the
  // Probe's Circuit Run History at the 240px minimum width.
  circuit_run_history: {
    entries: [
      { id: 9001, node_id: null, attempt: null, kind: 'configuration_pinned',
        detail: JSON.stringify({ behavior_revision: 1, graph_sha256: 'abcdef0123456789', reviewers: [{ node_id: 'reviewer' }] }),
        source: 'run.configuration', disposition: 'applied', observed_at: '2026-01-01T00:00:00Z' },
      { id: 9002, node_id: null, attempt: null, kind: 'queue_wait',
        detail: JSON.stringify({ reason: 'mesh_capacity', capacity: 2 }),
        source: 'circuit_worker.admission', disposition: 'waiting', observed_at: '2026-01-01T00:00:01Z' },
      { id: 9003, node_id: 'spawn', attempt: 1, kind: 'step_capacity_wait',
        detail: JSON.stringify({ before: null, after: JSON.stringify({ circuit_limit: true, agent_limit: false }) }),
        source: 'circuit_worker.capacity', disposition: 'waiting', observed_at: '2026-01-01T00:00:02Z' },
      { id: 9004, node_id: 'reviewer', attempt: 1, kind: 'evidence_window_changed',
        detail: JSON.stringify({ before: null, after: { attempt: '1', timeout_ms: '60000', since_ms: '1767225600000', observed: '0', explicit_budget: '0' } }),
        source: 'circuit_worker.reconciliation', disposition: 'waiting', observed_at: '2026-01-01T00:00:03Z' },
      { id: 9005, node_id: 'open_pr', attempt: 1, kind: 'operator_attestation',
        detail: 'Operator-recorded outcome (NotPerformed): Confirmed request was rejected before dispatch',
        source: 'operator', disposition: 'not_performed', observed_at: '2026-01-01T00:00:04Z' },
      { id: 9006, node_id: null, attempt: null, kind: 'recovery',
        detail: JSON.stringify({ successor_run_id: 88, rounds: 2 }),
        source: 'operator', disposition: 'applied', observed_at: '2026-01-01T00:00:05Z' },
    ],
    checkpoints: [],
    coverage: [],
  },
};
