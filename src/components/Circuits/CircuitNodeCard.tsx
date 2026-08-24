/**
 * CircuitNodeCard — the canvas editor's custom React Flow node card
 * (issue #1209).
 *
 * Presentation per the spec: category accent stripe, glyph, label,
 * stable id, one-line config summary. Gate nodes expose named outcome
 * source handles (`OnOutcome` ports); every other kind gets a single
 * anonymous source handle. Live-run overlays ride on top: running
 * pulses, completed shows a check + duration, blocked parks an amber
 * Approve badge, failures surface their error via tooltip.
 */

import { Handle, Position, type Node, type NodeProps } from '@xyflow/react';
import type { CircuitNode } from '../../types/generated/CircuitNode';
import {
  categoryAccent,
  categoryOf,
  configSummary,
  sourceOutcomes,
  specFor,
  stepDurationMs,
  type StepLike,
} from './circuitGraphModel';

/** The editor's custom React Flow node shape. */
export interface CircuitNodeCardData extends Record<string, unknown> {
  circuitNode: CircuitNode;
  /** This node's step in the selected run, if any. */
  step?: StepLike;
  /** Run id to approve when this node is parked in `blocked`. */
  blockedRunId?: number;
  onApprove?: (runId: number, nodeId: string) => void;
}

export type CircuitFlowNode = Node<CircuitNodeCardData, 'circuit'>;

function CategoryGlyph({ category, className }: { category: string; className: string }) {
  const stroke = { fill: 'none', stroke: 'currentColor', strokeWidth: 2 } as const;
  return (
    <svg className={className} viewBox="0 0 24 24" aria-hidden>
      {category === 'trigger' && <path d="M13 2 4 14h6l-1 8 9-12h-6l1-8z" {...stroke} strokeLinejoin="round" />}
      {category === 'action' && <path d="M6 4l14 8-14 8V4z" {...stroke} strokeLinejoin="round" />}
      {category === 'gate' && (
        <path d="M12 3l8 4v5c0 5-3.5 8-8 9-4.5-1-8-4-8-9V7l8-4z" {...stroke} strokeLinejoin="round" />
      )}
      {category === 'join' && (
        <>
          <path d="M6 4v5a6 6 0 0 0 6 6h6M18 15l-3-3m3 3l-3 3" {...stroke} strokeLinecap="round" strokeLinejoin="round" />
        </>
      )}
    </svg>
  );
}

/** Status overlay vocabulary → Tailwind token classes. */
function statusOverlay(step: StepLike): { ring: string; badge: string | null } {
  switch (step.status) {
    case 'running':
      return { ring: 'ring-2 ring-accent-cyan animate-pulse', badge: 'running' };
    case 'completed':
      return { ring: 'ring-2 ring-status-success', badge: 'completed' };
    case 'blocked':
      return { ring: 'ring-2 ring-status-warning', badge: 'blocked' };
    case 'failed':
    case 'cancelled':
      return { ring: 'ring-2 ring-status-error', badge: step.status };
    default:
      return { ring: '', badge: null };
  }
}

export function CircuitNodeCard({ data }: NodeProps<CircuitFlowNode>) {
  const { circuitNode, step, blockedRunId, onApprove } = data;
  const category = categoryOf(circuitNode.type);
  const accent = categoryAccent(category);
  const outcomes = sourceOutcomes(circuitNode.type);
  const overlay = step ? statusOverlay(step) : null;
  const duration = step ? stepDurationMs(step) : null;

  return (
    <div
      data-testid={`circuit-node-${circuitNode.id}`}
      className={`
        w-[220px] rounded-md border border-border-subtle bg-bg-card shadow-md
        ${overlay?.ring ?? ''} ${data.selected ? 'border-border-active' : ''}
      `}
    >
      <Handle
        type="target"
        position={Position.Left}
        className="!bg-border-strong"
        aria-label={`${circuitNode.id} input`}
      />

      {/* Category accent stripe */}
      <div className={`flex items-center gap-2 px-2 py-1.5 rounded-t-md ${accent.bg}/15`}>
        <span className={`${accent.text}`}>
          <CategoryGlyph category={category} className="w-4 h-4" />
        </span>
        <span className={`text-xs font-semibold truncate ${accent.text}`}>
          {specFor(circuitNode.type.type).label}
        </span>
      </div>

      <div className="px-2 py-1.5">
        <div className="text-2xs font-mono text-text-muted">{circuitNode.id}</div>
        <div className="text-xs text-text-secondary truncate" title={configSummary(circuitNode.type)}>
          {configSummary(circuitNode.type)}
        </div>

        {/* Live run overlay */}
        {overlay?.badge === 'running' && (
          <span
            className="mt-1 inline-flex items-center gap-1 text-2xs text-accent-cyan"
            data-testid={`node-running-${circuitNode.id}`}
          >
            ● running…
          </span>
        )}
        {overlay?.badge === 'completed' && (
          <span
            className="mt-1 inline-flex items-center gap-1 text-2xs text-status-success"
            data-testid={`node-completed-${circuitNode.id}`}
            title={duration !== null ? `${(duration / 1000).toFixed(1)}s` : undefined}
          >
            ✓{duration !== null ? ` ${(duration / 1000).toFixed(1)}s` : ''}
          </span>
        )}
        {(overlay?.badge === 'failed' || overlay?.badge === 'cancelled') && (
          <span
            className="mt-1 inline-block text-2xs text-status-error truncate max-w-full"
            data-testid={`node-error-${circuitNode.id}`}
            title={step?.error_message ?? ''}
          >
            ⚠ failed — hover for error
          </span>
        )}
        {overlay?.badge === 'blocked' && (
          <span
            className="mt-1 inline-flex items-center gap-1 px-1 rounded-sm bg-status-warning/15 text-status-warning text-2xs"
            data-testid={`node-blocked-${circuitNode.id}`}
          >
            waiting for approval
            {blockedRunId !== undefined && onApprove && (
              <button
                type="button"
                onClick={() => onApprove(blockedRunId, circuitNode.id)}
                data-testid={`node-approve-${circuitNode.id}`}
                className="px-1 rounded-sm bg-status-warning/25 hover:bg-status-warning/40 font-semibold"
              >
                Approve
              </button>
            )}
          </span>
        )}
      </div>

      {/* Source handles: gates that route by outcome get named ports. */}
      {outcomes ? (
        outcomes.map((outcome, i) => (
          <Handle
            key={outcome}
            id={outcome}
            type="source"
            position={Position.Right}
            style={{ top: `${30 + i * 16}%` }}
            className="!bg-border-strong"
            aria-label={`${circuitNode.id} on ${outcome}`}
            data-testid={`handle-${circuitNode.id}-${outcome}`}
          />
        ))
      ) : (
        <Handle
          type="source"
          position={Position.Right}
          className="!bg-border-strong"
          aria-label={`${circuitNode.id} output`}
        />
      )}

      {/* Named-port legend for gate cards */}
      {outcomes && outcomes.length > 1 && (
        <div className="px-2 pb-1 flex flex-col items-end gap-0.5">
          {outcomes.map((o) => (
            <span key={o} className="text-2xs text-text-muted">
              {o}
            </span>
          ))}
        </div>
      )}
    </div>
  );
}
