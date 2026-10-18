import { NodeRequestPreview } from './NodeRequestPreview';

import type { AgentNode, NodeCircuitRun, Provider } from '../../api';

import { circuitStatusLine, needsYou } from '../../circuitRun';
import { ProviderIcon } from '../../../components/Providers/ProviderIcon';

import { type NodeInputRequest } from '../../../lib/status';
import type { SentAction } from './types';

// Triage deck card (issue #1377, post-review rewrite; request semantics
// #1966): one awaiting-input node with its context (mesh/repo, branch, request
// text) and the reply controls its request actually supports.
//
// The controls are chosen from the normalized lifecycle kind, never from the
// bare `awaiting_input` status (issue #1966). `y`/`n` are a guess about a
// harness's own approval prompt, so they ship only against a
// `permission_requested` observation. A question gets an explicit
// open-to-answer action — the harness's question UI is where its answer is
// actually accepted, and Buildmesh cannot tell a keystroke apart from an
// accepted answer. Enumerated choices, when the harness supplied them, are
// shown as the answer list, never as pre-filled chips that would have to
// guess how the harness selects an option. An unobserved or unclassified
// request gets the same open-to-answer action: less evidence, same treatment.
//
// State machine (review feedback): `sent` is the SPECIFIC action the user
// took on THIS request — "approve" or "reject". When set, BOTH chips are
// disabled (the agent already saw the CR/LF — a retraction would either be
// ignored or worse, send an opposite prompt into a stream the agent has
// already moved past). The chip whose action was sent shows the success
// label; the other stays greyed out. `busy` (in-flight POST) still takes
// precedence over `sent` so the "Sending…" feedback isn't lost. Both are
// resolved by the parent against the request key, so a replacement request
// arrives with live controls instead of inheriting this one's disabled state.
//
// The previous design had a separate "Focus terminal" chip alongside the
// card-body tap target — two competing buttons doing the same thing on a
// 120px card, and they took width away from the action chips. Dropped while
// Approve/Reject owned the chip row (issue #1377); restored as the single
// labelled control now that question and unknown requests have no quick
// action to offer, because an unlabelled card body does not tell the user
// that answering means opening the terminal (issue #1966).
export function AttentionCard({
  node,
  circuitRun,
  meshName,
  request,
  providers,
  busy,
  sent,
  onApprove,
  onReject,
  onOpen,
}: {
  node: AgentNode;
  circuitRun?: NodeCircuitRun;
  meshName?: string;
  request: NodeInputRequest;
  providers?: Provider[];
  busy?: SentAction;
  sent?: SentAction;
  onApprove: () => void;
  onReject: () => void;
  onOpen: () => void;
}) {
  // Same live-provider lookup contract as `NodeRow` (issue #328): the label
  // and chip colour come from the fetched `listProviders()` payload, with a
  // deterministic raw-id fallback before it resolves.
  const providerMeta = providers?.find(
    (p) => p.id === (node.launch_configuration?.id ?? node.provider),
  );
  const providerLabel =
    node.launch_configuration?.name ?? providerMeta?.label ?? node.provider;
  // BOTH chips disable when an action was taken (or is in flight) on this
  // card — see the state machine comment above. `sent` (enum) gives us the
  // strict superset that the previous boolean `sent` couldn't: the
  // disabled check is a one-liner, no separate per-chip sent state to
  // reconcile.
  const chipsDisabled = busy !== undefined || sent !== undefined;
  const approveLabel =
    busy === "approve"
      ? "Sending…"
      : sent === "approve"
        ? "Approved ✓"
        : "Approve (Y)";
  const rejectLabel =
    busy === "reject"
      ? "Sending…"
      : sent === "reject"
        ? "Rejected ✗"
        : "Reject (N)";
  // A permission decision is the only request with a yes/no answer Buildmesh
  // can honestly put on a chip.
  const isPermission = request.mode === "permission";
  return (
    <div className="deck-card" data-testid={`attn-card-${node.id}`}>
      <button
        type="button"
        className="deck-body"
        data-testid={`node-${node.id}`}
        aria-label={`Open ${node.name} terminal to respond`}
        onClick={onOpen}
      >
        <div style={{ display: "flex", alignItems: "center", gap: 8 }}>
          <ProviderIcon
            providerId={node.provider}
            withBackground
            backgroundColor={providerMeta?.color}
            fallbackGlyph={providerMeta?.icon}
            chipTestId="node-avatar"
            title={providerLabel}
            className="h-4 w-4"
          />
          <span
            style={{
              flex: 1,
              minWidth: 0,
              fontSize: 14,
              fontWeight: 500,
              color: "var(--text)",
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
              textAlign: "left",
            }}
          >
            {node.name}
          </span>
        </div>
        <div
          style={{
            fontSize: 11,
            color: "var(--text-faint)",
            marginTop: 4,
            overflow: "hidden",
            textOverflow: "ellipsis",
            whiteSpace: "nowrap",
            textAlign: "left",
          }}
        >
          Mesh: {meshName ?? "Unknown"}
        </div>
        <div className="node-attention-meta">
          {node.branch ? `⎇ ${node.branch}` : "No branch reported"} ·{" "}
          {providerLabel}
        </div>
        {circuitRun && <div className="node-circuit-line" data-testid={`node-circuit-${node.id}`} data-needs-you={needsYou(circuitRun)}>{circuitStatusLine(circuitRun)}</div>}
        <NodeRequestPreview nodeId={node.id} request={request} />
      </button>
      {request.choices.length > 0 && (
        // The answer list the harness offered, as text. Presentational on
        // purpose: tapping a label cannot be verified to select the option
        // in the harness's own prompt, so a chip here would promise a
        // one-tap answer Buildmesh cannot deliver (issue #1966). It sits
        // outside the card-body button because a button may only contain
        // phrasing content, and this list is not a control.
        <ul data-testid={`attn-choices-${node.id}`} className="attn-choices">
          {request.choices.map((choice) => (
            <li key={choice}>{choice}</li>
          ))}
        </ul>
      )}
      <div className="deck-chips">
        {isPermission ? (
          <>
            <button
              type="button"
              className="deck-chip approve"
              data-testid={`attn-approve-${node.id}`}
              disabled={chipsDisabled}
              onClick={onApprove}
            >
              {approveLabel}
            </button>
            <button
              type="button"
              className="deck-chip reject"
              data-testid={`attn-reject-${node.id}`}
              disabled={chipsDisabled}
              onClick={onReject}
            >
              {rejectLabel}
            </button>
          </>
        ) : (
          <button
            type="button"
            className="deck-chip"
            data-testid={`attn-open-${node.id}`}
            onClick={onOpen}
          >
            {request.mode === "question"
              ? "Answer in terminal"
              : "Open terminal to respond"}
          </button>
        )}
      </div>
    </div>
  );
}
