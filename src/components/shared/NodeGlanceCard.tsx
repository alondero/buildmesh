import { useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import type { NodeGlanceState } from '../../hooks/useNodeGlance';
import { getStatusConfig } from '../../lib/status';
import {
  activityHeadline, activityLine, glanceTimings, timingLine, toolCountLine, unavailableCopy,
} from '../../lib/nodeGlance';

/**
 * The body of a Node Glance: what the agent is doing, how long it has been at
 * it, what it last said and what it was asked. Read-only; the terminal stays
 * the place to act.
 */
export function NodeGlanceSummary({ state }: { state: NodeGlanceState }) {
  const { glance, error, sinceReadMs } = state;
  if (!glance) {
    return <p className="text-xs text-text-secondary">{error ? `Could not read this agent: ${error}` : 'Reading the agent…'}</p>;
  }
  const transcript = glance.transcript;
  const headline = activityHeadline(glance);
  const earlier = transcript?.activity.slice(0, -1) ?? [];
  const tools = toolCountLine(glance);
  return (
    <div className="flex flex-col gap-2 text-xs" data-testid="node-glance">
      <p className="text-text-secondary">{timingLine(getStatusConfig(glance.status).label, glanceTimings(glance, sinceReadMs))}</p>
      {headline && <p className="break-words font-mono text-text-primary">{headline}</p>}
      {earlier.length > 0 && (
        <ul aria-label="Earlier activity" className="flex flex-col gap-0.5 font-mono text-text-muted">
          {earlier.map((activity, index) => <li key={index} className="truncate" title={activityLine(activity)}>{activityLine(activity)}</li>)}
        </ul>
      )}
      {tools && <p className="text-text-muted">{tools}</p>}
      {transcript?.last_message && (
        <section aria-label="Last message">
          <h3 className="text-2xs font-medium uppercase tracking-wide text-text-muted">
            {transcript.message_after_prompt ? 'Last message' : 'Last message (earlier task)'}
          </h3>
          <p className="line-clamp-5 whitespace-pre-wrap break-words text-text-secondary">{transcript.last_message}</p>
        </section>
      )}
      {transcript?.last_prompt && (
        <section aria-label="Task">
          <h3 className="text-2xs font-medium uppercase tracking-wide text-text-muted">Task</h3>
          <p className="line-clamp-3 whitespace-pre-wrap break-words text-text-secondary">{transcript.last_prompt}</p>
        </section>
      )}
      {glance.transcript_unavailable && <p className="text-text-muted">{unavailableCopy(glance.transcript_unavailable)}</p>}
      {error && <p className="text-text-muted">Last refresh failed: {error}</p>}
    </div>
  );
}

const CARD_WIDTH = 320;
const GUTTER = 8;

/**
 * A glance beside `anchor`, portalled to the body so no transformed or
 * filtered ancestor can re-anchor it (see the overlay anti-pattern in the
 * architecture index). Pointer entry keeps it open so the text can be read
 * and selected.
 */
export function NodeGlanceHoverCard({ anchor, id, name, state, onPointerEnter, onPointerLeave }: {
  anchor: HTMLElement;
  id: string;
  name: string;
  state: NodeGlanceState;
  onPointerEnter: () => void;
  onPointerLeave: () => void;
}) {
  const cardRef = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState<{ top: number; left: number } | null>(null);
  useLayoutEffect(() => {
    const rect = anchor.getBoundingClientRect();
    const height = cardRef.current?.offsetHeight ?? 0;
    const right = rect.right + GUTTER;
    const left = right + CARD_WIDTH <= window.innerWidth - GUTTER
      ? right
      : Math.max(GUTTER, rect.left - CARD_WIDTH - GUTTER);
    const top = Math.max(GUTTER, Math.min(rect.top, window.innerHeight - height - GUTTER));
    setPosition(current => (current?.top === top && current.left === left ? current : { top, left }));
  }, [anchor, state]);
  return createPortal(
    <div
      ref={cardRef}
      id={id}
      role="tooltip"
      onPointerEnter={onPointerEnter}
      onPointerLeave={onPointerLeave}
      style={{ top: position?.top ?? 0, left: position?.left ?? 0, width: CARD_WIDTH, visibility: position ? 'visible' : 'hidden' }}
      className="fixed z-[90] max-h-[60vh] overflow-y-auto rounded-md border border-border-subtle bg-bg-overlay p-3 shadow-md"
    >
      <p className="mb-2 truncate text-sm font-medium text-text-primary">{name}</p>
      <NodeGlanceSummary state={state} />
    </div>,
    document.body,
  );
}
