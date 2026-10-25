import type { AgentNode } from '../../stores/agentNodeStore';
import type { CircuitAgentOwnership } from '../../types/generated/CircuitAgentOwnership';
import type { RunOverviewState } from '../Circuits/useCircuitRunOverview';
import { useProviderList } from '../../hooks/useProviderList';
import { isTerminalRunState, runStateLabel } from '../Circuits/circuitVocabulary';
import { statusTextClass } from '../Circuits/circuitGraphModel';
import { overviewNeedsYou, railRows } from '../Circuits/runOverviewModel';
import { nodeRoleLabel, parseRunContext } from '../Circuits/runStepPresentation';
import { reviewCircuitMetadata, reviewResult } from '../Circuits/runDiagnostics';
import { reviewRoundLabel } from '../Circuits/ReviewRoundLimit';
import { AgentReviewButton } from './AgentReviewButton';

/** A compact entry point beside the terminal, sharing the card's run snapshot. */
export function NodeCircuitSummary({ node, ownership, resource, onOpenOverview }: {
  node: AgentNode;
  ownership?: CircuitAgentOwnership;
  resource: RunOverviewState;
  onOpenOverview: () => void;
}) {
  const providers = useProviderList();
  const { overview, error, refresh } = resource;
  const state = overview?.run.state ?? ownership?.state;
  const context = parseRunContext(overview?.run.context_json ?? '{}');
  const roundContext = overview?.review_round_limit == null ? context : {
    ...context, 'review.round_limit': String(overview.review_round_limit),
  };
  const review = overview
    ? reviewResult(overview, reviewCircuitMetadata({ graph_json: JSON.stringify(overview.graph) }), context)
    : null;
  const needsYou = overview !== null && overviewNeedsYou(overview);
  const current = overview
    ? railRows(overview.graph, overview.steps, overview.run.state).filter(row => row.state === 'current')
    : [];

  return (
    <section aria-label="Node review circuit" className="shrink-0 border-b border-border-subtle bg-bg-card px-3 py-2 text-xs">
      <div className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <span className="min-w-0 max-w-full break-words font-medium text-text-primary">{ownership ? ownership.circuit_name : 'Review circuit'}</span>
        {state ? <>
          <span className={statusTextClass(review?.needsAttention ? 'failed' : state)}>{review?.label ?? runStateLabel(state)}</span>
          {(context['retry.attempt'] || overview?.review_round_limit != null) && (
            <span className="text-text-secondary">{reviewRoundLabel(roundContext)}</span>
          )}
          {needsYou && <span className="text-status-warning">Needs attention</span>}
          <button type="button" onClick={onOpenOverview}
            className="min-h-6 text-accent-cyan hover:underline">View circuit</button>
        </> : <span className="text-text-secondary">No circuit attached</span>}
        {(!state || isTerminalRunState(state)) && <AgentReviewButton node={node} providerList={providers} label="Add review" />}
      </div>
      {current.length > 0 && <p className="mt-1 break-words text-text-secondary">
        {current.map(row => nodeRoleLabel(row.nodeId, row.kind)).join(' · ')}
      </p>}
      {ownership && overview === null && error === null && <p className="mt-1 text-text-muted">Loading circuit details…</p>}
      {error !== null && <div className="mt-1 flex flex-wrap items-center gap-2">
        <p role="alert" className="min-w-0 break-words text-status-error">Could not refresh circuit: {error}</p>
        <button type="button" onClick={refresh} className="min-h-6 text-accent-cyan hover:underline">Retry</button>
      </div>}
    </section>
  );
}
