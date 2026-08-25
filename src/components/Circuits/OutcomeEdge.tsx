/**
 * OutcomeEdge — the canvas editor's custom React Flow edge (issue #1209).
 *
 * Renders an interactive condition badge at the edge midpoint:
 * `Always` or `OnOutcome(...)`. Clicking cycles the condition through
 * the source gate's outcome vocabulary, so wiring a classifier branch
 * never leaves the canvas.
 */

import {
  BaseEdge,
  EdgeLabelRenderer,
  getSmoothStepPath,
  type Edge,
  type EdgeProps,
} from '@xyflow/react';
import type { EdgeCondition } from '../../types/generated/EdgeCondition';
import { conditionLabel, edgeKey } from './circuitGraphModel';

/** The custom edge's payload. */
export interface OutcomeEdgeData extends Record<string, unknown> {
  condition: EdgeCondition;
  highlight?: boolean;
  /** Owned by the editor: cycles this edge's condition in ITS state
   *  (the editor renders fully controlled, so the edge must never touch
   *  React Flow's internal store directly). */
  onCycle?: (edgeId: string) => void;
}

export type OutcomeEdgeType = Edge<OutcomeEdgeData, 'outcome'>;

/** Cycle order for a gate with these named outcomes; plain nodes only
 *  toggle Always ↔ OnOutcome(completed). Outcome conditions compare by
 *  their payload — the editor rebuilds `{ on_outcome }` objects every
 *  render, so reference equality would never match and every badge
 *  would collapse back to `always`. */
export function nextCondition(current: EdgeCondition, outcomes: EdgeCondition[]): EdgeCondition {
  const cycle: EdgeCondition[] = ['always', ...outcomes];
  const idx = cycle.findIndex((c) =>
    c === 'always' ? current === 'always' : current !== 'always' && c.on_outcome === current.on_outcome
  );
  return cycle[(idx + 1) % cycle.length];
}

export function OutcomeEdge({
  id,
  source,
  target,
  sourceX,
  sourceY,
  targetX,
  targetY,
  sourcePosition,
  targetPosition,
  data,
}: EdgeProps<OutcomeEdgeType>) {
  const [path] = getSmoothStepPath({
    sourceX,
    sourceY,
    targetX,
    targetY,
    sourcePosition,
    targetPosition,
  });
  const condition: EdgeCondition = (data?.condition as EdgeCondition) ?? 'always';
  const label = conditionLabel(condition);
  const highlight: boolean = Boolean(data?.highlight);
  // Key-shaped id so tests (and the traversal highlighter) can address
  // the edge by its CURRENT condition.
  const key = edgeKey({ from: source, to: target, condition });

  return (
    <>
      <BaseEdge
        id={id}
        path={path}
        style={highlight ? { stroke: 'var(--color-accent-cyan)', strokeWidth: 3 } : undefined}
      />
      <EdgeLabelRenderer>
        <button
          type="button"
          data-testid={`edge-badge-${key}`}
          onClick={() => data?.onCycle?.(id)}
          className={`
            nodrag nopan absolute px-1 rounded-sm text-2xs font-mono border pointer-events-auto
            ${
              highlight
                ? 'bg-accent-cyan/25 text-accent-cyan border-accent-cyan'
                : 'bg-bg-overlay text-text-muted border-border-subtle hover:text-text-primary'
            }
          `}
          style={{
            transform: `translate(-50%, -50%) translate(${(sourceX + targetX) / 2}px, ${(sourceY + targetY) / 2}px)`,
          }}
        >
          {label}
        </button>
      </EdgeLabelRenderer>
    </>
  );
}
