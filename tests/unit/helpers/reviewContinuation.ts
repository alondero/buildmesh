import type { ReviewContinuation } from '../../../src/types/generated/ReviewContinuation';

/** A server response fixture. Availability is an explicit backend fact. */
export function reviewContinuation(overrides: Partial<ReviewContinuation> = {}): ReviewContinuation {
  return {
    lineage: { review_origin: 'local_preset', extended: false, recovery: { kind: 'none' } },
    supported: true,
    available: true,
    refusal_reason: null,
    refusal_message: null,
    source_agent_node_id: 42,
    reviewer_agent_node_id: 43,
    target_run_id: 11,
    review_nodes: {
      verdict_node_id: 'verdict',
      retry_node_ids: ['retry'],
      merge_node_id: 'merge',
      merge_confirmed_node_id: null,
      merge_unconfirmed_node_ids: [],
    },
    ...overrides,
  };
}
