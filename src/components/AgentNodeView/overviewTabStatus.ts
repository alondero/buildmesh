import type { CircuitAgentOwnership } from '../../types/generated/CircuitAgentOwnership';
import type { RunOverviewState } from '../Circuits/useCircuitRunOverview';
import { runStateLabel } from '../Circuits/circuitVocabulary';
import { statusTextClass } from '../Circuits/circuitGraphModel';
import { railRows } from '../Circuits/runOverviewModel';
import { nodeRoleLabel, parseRunContext } from '../Circuits/runStepPresentation';
import { reviewCircuitMetadata, reviewResult } from '../Circuits/runDiagnostics';
import { reviewRound, reviewRoundLabel } from '../Circuits/ReviewRoundLimit';

export interface OverviewTabStatus {
  /** Visible tab text: run state, plus the review round when the run has one. */
  label: string;
  /** Status colour for the label. */
  className: string;
  /** Circuit name, round, current steps and refresh state, for the tooltip. */
  detail: string;
}

/**
 * What a card's Overview tab says about its Circuit Run. The tab is the card's
 * only circuit status surface (DESIGN.md principle 6): it shares the card's run
 * snapshot, so a known ownership state shows while details load or fail.
 */
export function overviewTabStatus(ownership: CircuitAgentOwnership | undefined, resource: RunOverviewState): OverviewTabStatus | null {
  const { overview, error } = resource;
  const state = overview?.run.state ?? ownership?.state;
  if (!state) return null;
  const context = parseRunContext(overview?.run.context_json ?? '{}');
  const roundContext = overview?.review_round_limit == null ? context : {
    ...context, 'review.round_limit': String(overview.review_round_limit),
  };
  const review = overview
    ? reviewResult(overview, reviewCircuitMetadata({ graph_json: JSON.stringify(overview.graph) }), context)
    : null;
  const stateLabel = review?.label ?? runStateLabel(state);
  const hasRound = !!context['retry.attempt'] || overview?.review_round_limit != null;
  const { attempt, limit } = reviewRound(roundContext);
  const current = overview
    ? railRows(overview.graph, overview.steps, overview.run.state).filter(row => row.state === 'current')
    : [];
  const detail = [
    overview?.circuit_name ?? ownership?.circuit_name,
    stateLabel,
    hasRound ? reviewRoundLabel(roundContext) : null,
    ...current.map(row => nodeRoleLabel(row.nodeId, row.kind)),
    error !== null ? `Could not refresh circuit: ${error}` : overview === null ? 'Loading circuit details…' : null,
  ].filter(Boolean).join(' · ');
  const round = limit ? `${attempt}/${limit === 'unlimited' ? '∞' : limit}` : `round ${attempt}`;
  return {
    label: hasRound ? `${stateLabel} · ${round}` : stateLabel,
    className: statusTextClass(review?.needsAttention ? 'failed' : state),
    detail,
  };
}
