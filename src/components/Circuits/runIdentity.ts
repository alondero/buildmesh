/**
 * runIdentity — who a circuit run is about, and when it really ran.
 *
 * A run id (`#335`) and a circuit node id (`review_classifier`) are scheduler
 * vocabulary: they say nothing about the work. What a person scans for is the
 * agent node doing the implementation, so the run is named after it, falling
 * back to the issue/PR it came from once that node has been closed.
 *
 * Timing leaves out the time a run spent in the admission queue. The run row
 * is stamped when its trigger fires, but nothing happens until a slot frees, so
 * the first step's start is the moment Autopilot actually began (verified
 * against the `running` run transition in the ledger). Pure and React-free.
 */

import { isTerminalRunState, ledgerTimestampMs } from './circuitGraphModel';
import type { NodeIndex } from './runStepPresentation';
import type { AutopilotCircuitRun } from '../../types/generated/AutopilotCircuitRun';
import type { AutopilotCircuitRunStep } from '../../types/generated/AutopilotCircuitRunStep';

type StepTimes = Pick<AutopilotCircuitRunStep, 'started_at'>;

/** Epoch-ms when Autopilot began work on the run, or `null` while it has not. */
export function runStartedMs(steps: ReadonlyArray<StepTimes>): number | null {
  let earliest = Number.POSITIVE_INFINITY;
  for (const step of steps) {
    if (step.started_at === null) continue;
    const started = ledgerTimestampMs(step.started_at);
    if (Number.isFinite(started) && started < earliest) earliest = started;
  }
  return Number.isFinite(earliest) ? earliest : null;
}

/** Epoch-ms the run reached a terminal state, or `null` while it is live. */
export function runFinishedMs(run: Pick<AutopilotCircuitRun, 'state' | 'updated_at'>): number | null {
  if (!isTerminalRunState(run.state)) return null;
  const finished = ledgerTimestampMs(run.updated_at);
  return Number.isFinite(finished) ? finished : null;
}

/** How long Autopilot actually worked on the run (queue time excluded). */
export function runActiveDurationMs(
  run: Pick<AutopilotCircuitRun, 'state' | 'updated_at'>,
  steps: ReadonlyArray<StepTimes>,
  now: Date,
): number | null {
  const started = runStartedMs(steps);
  if (started === null) return null;
  const end = runFinishedMs(run) ?? now.getTime();
  return Math.max(0, end - started);
}

const MONTHS = ['Jan', 'Feb', 'Mar', 'Apr', 'May', 'Jun', 'Jul', 'Aug', 'Sep', 'Oct', 'Nov', 'Dec'];

function zoned(ms: number, timeZone: string | undefined) {
  const parts = new Intl.DateTimeFormat('en-GB', {
    timeZone,
    hourCycle: 'h23',
    year: 'numeric',
    month: 'numeric',
    day: 'numeric',
    hour: '2-digit',
    minute: '2-digit',
    second: '2-digit',
  }).formatToParts(new Date(ms));
  const get = (type: string) => Number(parts.find((part) => part.type === type)?.value ?? 0);
  return {
    year: get('year'),
    month: get('month'),
    day: get('day'),
    time: [get('hour'), get('minute'), get('second')].map((n) => String(n).padStart(2, '0')).join(':'),
  };
}

/**
 * Local wall-clock time of an instant: `14:48:45` for today, `4 Oct 09:05:07`
 * for any other day. `timeZone` exists so tests are not bound to the host.
 */
export function formatWallClock(ms: number, now: Date, timeZone?: string): string {
  const at = zoned(ms, timeZone);
  const today = zoned(now.getTime(), timeZone);
  if (at.year === today.year && at.month === today.month && at.day === today.day) return at.time;
  return `${at.day} ${MONTHS[at.month - 1]} ${at.time}`;
}

export interface RunSubject {
  /** What to call the run; `null` when nothing specific is recorded. */
  label: string | null;
  /** The agent node to link to, when it still exists. */
  agentNodeId: number | null;
}

/** Spawn steps that run a reviewer rather than the implementation. */
function reviewerNodeIds(nodeIndex: NodeIndex): Set<string> {
  const reviewers = new Set<string>();
  for (const node of nodeIndex.values()) {
    if (node.type.type === 'review_verdict' && node.type.target_node_id) {
      reviewers.add(node.type.target_node_id);
    }
  }
  return reviewers;
}

function recordedLabel(context: Record<string, string>): string | null {
  const title = (context['issue.title'] ?? '').trim();
  if (title !== '') {
    const number = (context['issue.number'] ?? '').trim();
    return number === '' ? title : `#${number} ${title}`;
  }
  const source = (context['source.name'] ?? '').trim();
  if (source !== '') return source;
  const pr = (context['pr.title'] ?? '').trim();
  return pr !== '' ? pr : null;
}

/**
 * Name a run after its implementation node. A title-bar review is about the
 * borrowed source agent; an issue-driven run is about its `implementer`, never
 * the reviewer that happens to be active. When that agent node has been closed
 * the run keeps a recognisable name from what it recorded (issue, source, PR).
 */
export function runSubject(
  run: Pick<AutopilotCircuitRun, 'source_agent_node_id'>,
  steps: ReadonlyArray<Pick<AutopilotCircuitRunStep, 'node_id' | 'agent_node_id'>>,
  context: Record<string, string>,
  nodeIndex: NodeIndex,
  agentName: (nodeId: number) => string | null,
): RunSubject {
  const reviewers = reviewerNodeIds(nodeIndex);
  // A node the blueprint no longer defines is still the best guess for the
  // implementation: only a known non-spawn node or a known reviewer is ruled out.
  const implementation = steps.find((step) => {
    const kind = nodeIndex.get(step.node_id)?.type.type;
    return (
      step.agent_node_id !== null &&
      (kind === undefined || kind === 'spawn_agent_node') &&
      !reviewers.has(step.node_id)
    );
  });
  const candidate = run.source_agent_node_id ?? implementation?.agent_node_id ?? null;
  if (candidate !== null) {
    const name = agentName(candidate);
    if (name !== null) return { label: name, agentNodeId: candidate };
  }
  return { label: recordedLabel(context), agentNodeId: null };
}
