/**
 * runStepPresentation — the plain-English reading of a single circuit run
 * STEP.
 *
 * `runDiagnostics.ts` answers "what is this run doing?". This module answers
 * the per-step questions the run card previously left encoded in raw ledger
 * vocabulary: which role is this node, what did its gate actually decide, and
 * which pass of a retry loop is it? The circuit `node_id` (`review_classifier`,
 * `implementer`) and the raw `attempt`/`outcome` tokens are scheduler
 * vocabulary — fine for logs, not for a person scanning history.
 *
 * Pure and React-free, mirroring `runDiagnostics.ts`, so the copy is
 * unit-testable without mounting the Probe.
 *
 * The interesting data source is the run's `context_json`. The stepper writes
 * `node.<id>.review_verdict` (`approved` | `changes_requested` | `blocked`),
 * `node.<id>.review_verdict_attempt`, and `node.<id>.evaluated_output` (the
 * text the gate classified — for a `review_verdict` that is the reviewer's
 * report). None of that needs a backend change; it is already on
 * `CircuitRunDetail.run`. Retention empties `context_json` on pruned terminal
 * runs, so everything derived from it is best-effort by construction.
 */

import type { CircuitNode } from '../../types/generated/CircuitNode';
import type { CircuitNodeKind } from '../../types/generated/CircuitNodeKind';
import type { AutopilotCircuitRun } from '../../types/generated/AutopilotCircuitRun';
import type { AutopilotCircuitRunStep } from '../../types/generated/AutopilotCircuitRunStep';
import { specFor } from './circuitGraphModel';
import { isQueuedStepStatus } from './circuitVocabulary';

/** Circuit `node_id` → blueprint node, parsed once per circuit row. */
export type NodeIndex = ReadonlyMap<string, CircuitNode>;

export function indexNodes(nodes: readonly CircuitNode[]): NodeIndex {
  const byId = new Map<string, CircuitNode>();
  for (const node of nodes) byId.set(node.id, node);
  return byId;
}

/** Safe parse of a run's `context_json`. The ledger stores `{}` for a fresh
 *  run and pruned rows may hold nothing, so an unreadable payload is a
 *  legitimate "no context", never an error the caller must handle. */
export function parseRunContext(contextJson: string): Record<string, string> {
  try {
    const parsed: unknown = JSON.parse(contextJson);
    if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) return {};
    const out: Record<string, string> = {};
    for (const [key, value] of Object.entries(parsed)) {
      if (typeof value === 'string') out[key] = value;
    }
    return out;
  } catch {
    return {};
  }
}

const ROLE_LABELS: Partial<Record<CircuitNodeKind['type'], string>> = {
  manual: 'Manual trigger',
  interval: 'Interval trigger',
  github_issue_label: 'Issue trigger',
  github_pull_request_label: 'PR trigger',
  inject_pty: 'Send feedback',
  github_action: 'GitHub action',
  set_node_status: 'Set status',
  close_agent_node: 'Close agent',
  notify: 'Notify',
  llm_turn_classifier: 'Classifier',
  await_agent_turn: 'Await task',
  review_verdict: 'Review',
  deterministic_verification: 'Verification',
  collaborator_check: 'Approval gate',
  retry_limit: 'Retry budget',
  all_completed: 'All completed',
  any_completed: 'Any completed',
};

/** A human role for a circuit node. A spawned agent keeps its configured
 *  name when it has one (the review preset names the reviewer "Code
 *  reviewer"); otherwise the node id is the only honest identity we have. */
export function nodeRoleLabel(nodeId: string, kind: CircuitNodeKind | undefined): string {
  if (kind === undefined) return nodeId;
  if (kind.type === 'spawn_agent_node') {
    const name = kind.name?.trim();
    return name !== undefined && name !== '' ? name : nodeId;
  }
  // `specFor` owns the palette label for every kind; the table above just
  // overrides the few whose catalogue name is not the clearest at a glance.
  // Guarded: a kind the frontend does not know (wire drift / extension) must
  // read as its raw discriminator, never crash the Probe render tree.
  return ROLE_LABELS[kind.type] ?? specFor(kind.type)?.label ?? kind.type;
}

export type VerdictTone = 'success' | 'warning' | 'error';

export interface StepVerdict {
  label: string;
  tone: VerdictTone;
}

const VERDICT_CLASSES: Record<VerdictTone, string> = {
  success: 'text-status-success',
  warning: 'text-status-warning',
  error: 'text-status-error',
};

export function verdictTextClass(tone: VerdictTone): string {
  return VERDICT_CLASSES[tone];
}

/** The `node.<id>.review_verdict` token, when it belongs to the step's
 *  CURRENT attempt. Retries reuse the node id and overwrite the key, so an
 *  attempt mismatch means the value is a previous pass's verdict and must not
 *  be attributed to this row. */
function currentReviewVerdict(
  step: Pick<AutopilotCircuitRunStep, 'node_id' | 'attempt'>,
  context: Record<string, string>
): string | undefined {
  const recordedAttempt = context[`node.${step.node_id}.review_verdict_attempt`];
  if (recordedAttempt !== String(step.attempt)) return undefined;
  return context[`node.${step.node_id}.review_verdict`];
}

/**
 * The specific decision a gate step recorded, or `null` when nothing more
 * precise than the generic status label can be said. Returning `null` is
 * deliberate: a row that cannot be specific keeps its status label rather
 * than inventing a verdict.
 */
export function stepVerdict(
  step: Pick<AutopilotCircuitRunStep, 'node_id' | 'status' | 'outcome' | 'attempt'>,
  kind: CircuitNodeKind | undefined,
  context: Record<string, string>
): StepVerdict | null {
  if (kind === undefined) return null;
  switch (kind.type) {
    case 'review_verdict': {
      switch (currentReviewVerdict(step, context)) {
        case 'approved':
          return { label: 'APPROVED', tone: 'success' };
        case 'changes_requested':
          return { label: 'CHANGES REQUESTED', tone: 'warning' };
        case 'blocked':
          return { label: 'NEEDS ATTENTION', tone: 'error' };
      }
      // No attempt-matched context (pruned, or a hand-built ledger): the
      // recorded routing outcome is the next best signal. The stepper routes
      // a completed verdict to approval and a working one to "changes
      // requested"; blocked parks the run.
      switch (step.outcome) {
        case 'completed':
          return { label: 'APPROVED', tone: 'success' };
        case 'working':
          return { label: 'CHANGES REQUESTED', tone: 'warning' };
        case 'blocked':
          return { label: 'NEEDS ATTENTION', tone: 'error' };
        default:
          return null;
      }
    }
    case 'deterministic_verification':
      if (step.outcome === 'green') return { label: 'PASSED', tone: 'success' };
      if (step.outcome === 'red') return { label: 'FAILED', tone: 'error' };
      return null;
    case 'retry_limit':
      if (step.outcome === 'failed') return { label: 'LIMIT REACHED', tone: 'error' };
      return null;
    default:
      return null;
  }
}

/** `pass 2` once a node has been retried; `null` on the first pass so the
 *  common case stays quiet. */
export function stepPassLabel(
  step: Pick<AutopilotCircuitRunStep, 'attempt'>
): string | null {
  return step.attempt > 1 ? `pass ${step.attempt}` : null;
}

/**
 * The reviewer's report for a `review_verdict` step, when the run context
 * still carries it AND it belongs to this step's current attempt.
 *
 * Retries reuse the node id, so a report that has not been attempt-matched
 * would leak the previous pass's findings under the new pass. `null` when the
 * step is not a review gate, the context was pruned, or the recorded attempt
 * is not the step's — the card then offers no report rather than a stale one.
 */
export function reviewReport(
  step: Pick<AutopilotCircuitRunStep, 'node_id' | 'attempt'>,
  kind: CircuitNodeKind | undefined,
  context: Record<string, string>
): string | null {
  if (kind?.type !== 'review_verdict') return null;
  // `evaluated_output` and `review_verdict_attempt` are written together by
  // the stepper, so the attempt key is the receipt for the report.
  if (context[`node.${step.node_id}.review_verdict_attempt`] !== String(step.attempt)) return null;
  const report = context[`node.${step.node_id}.evaluated_output`];
  if (report === undefined || report.trim() === '') return null;
  return report;
}

/**
 * Which pass of the review loop the recorded verdict belongs to, from
 * `node.<verdictId>.review_verdict_attempt`. Used to turn the run-level
 * "Review approved" into "Review approved on pass 2". `null` when the run
 * predates the key or the context was pruned.
 */
export function reviewPassCount(
  context: Record<string, string>,
  verdictNodeId: string
): number | null {
  const raw = context[`node.${verdictNodeId}.review_verdict_attempt`];
  if (raw === undefined) return null;
  const parsed = Number.parseInt(raw, 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : null;
}

/** One-line summary of a step for the timeline, e.g.
 *  `Review · pass 2 · CHANGES REQUESTED`. Verdict and pass are omitted when
 *  the ledger cannot support them. */
export function stepSummary(
  step: Pick<AutopilotCircuitRunStep, 'node_id' | 'status' | 'outcome' | 'attempt'>,
  kind: CircuitNodeKind | undefined,
  context: Record<string, string>
): { role: string; pass: string | null; verdict: StepVerdict | null } {
  return {
    role: nodeRoleLabel(step.node_id, kind),
    pass: stepPassLabel(step),
    verdict: stepVerdict(step, kind, context),
  };
}

/**
 * The agent node a run card should link to. Preference order:
 *
 *  1. `source_agent_node_id` — the borrowed Agent Node a title-bar review
 *     borrowed, which is the relationship the user actually clicked from.
 *  2. the agent behind the run's current (running / blocked / queued) step —
 *     for issue-driven runs with no borrowed source, "who is working now".
 *  3. the **last** step that bound an agent, so a finished run still points at
 *     the node it drove most recently (the reviewer, not the original
 *     implementer).
 *
 * `null` when the run never bound an agent (a pure notify/trigger circuit).
 */
export function linkedAgentNodeId(
  run: Pick<AutopilotCircuitRun, 'source_agent_node_id'>,
  steps: ReadonlyArray<Pick<AutopilotCircuitRunStep, 'agent_node_id' | 'status'>>
): number | null {
  if (run.source_agent_node_id !== null) return run.source_agent_node_id;
  const active = steps.find(
    (step) =>
      step.agent_node_id !== null &&
      (step.status === 'running' || step.status === 'blocked' || isQueuedStepStatus(step.status))
  );
  if (active?.agent_node_id != null) return active.agent_node_id;
  for (let i = steps.length - 1; i >= 0; i -= 1) {
    const id = steps[i].agent_node_id;
    if (id !== null) return id;
  }
  return null;
}
