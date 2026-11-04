import type { NodeGlance } from '../types/generated/NodeGlance';
import type { NodeGlanceActivity } from '../types/generated/NodeGlanceActivity';
import type { TranscriptUnavailableReason } from '../types/generated/TranscriptUnavailableReason';

/**
 * Presentation for a Node Glance, shared by the desktop hover card and the
 * browser phone client. The Android client mirrors these rules in
 * `NodeGlance.kt`; `tests/unit/node-glance.test.ts` pins both the copy and the
 * timing arithmetic.
 */

/** Compact elapsed time: `45s`, `12m`, `3h 5m`, `2d 4h`. */
export function formatElapsed(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / 1000));
  if (seconds < 60) return `${seconds}s`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return minutes % 60 ? `${hours}h ${minutes % 60}m` : `${hours}h`;
  const days = Math.floor(hours / 24);
  return hours % 24 ? `${days}d ${hours % 24}h` : `${days}d`;
}

/** One activity line: the tool, and what it acted on when the call named it. */
export function activityLine(activity: NodeGlanceActivity): string {
  return activity.target ? `${activity.tool} · ${activity.target}` : activity.tool;
}

const ACTIVE = new Set(['running', 'spawning']);

/**
 * What the agent is doing right now, or did last. A running node's latest tool
 * call is current work; on any other status it is history.
 */
export function activityHeadline(glance: NodeGlance): string | null {
  const activity = glance.transcript?.activity ?? [];
  const latest = activity[activity.length - 1];
  if (!latest) return null;
  return `${ACTIVE.has(glance.status) ? 'Now' : 'Last'}: ${activityLine(latest)}`;
}

const UNAVAILABLE: Record<TranscriptUnavailableReason, string> = {
  unsupported: "This harness doesn't keep a transcript Buildmesh can read.",
  no_session: 'No session has been captured yet.',
  no_transcript: 'No transcript has been written yet.',
  unreadable: 'The transcript could not be read.',
  empty: 'Nothing has happened in this session yet.',
  shape_changed: "The transcript's format was not recognised.",
};

export function unavailableCopy(reason: TranscriptUnavailableReason): string {
  return UNAVAILABLE[reason];
}

export interface GlanceTimings {
  /** Time in the current status, e.g. `12m`. */
  inState: string | null;
  /** Age of the current agent session. */
  session: string | null;
  /** Time since the transcript was last written. */
  lastActivity: string | null;
}

/**
 * Elapsed times on the desktop's clock. `sinceReadMs` is how long the client
 * has held this glance, so the times keep counting between reads without
 * comparing two devices' clocks.
 */
export function glanceTimings(glance: NodeGlance, sinceReadMs = 0): GlanceTimings {
  const now = glance.observed_at_ms + Math.max(0, sinceReadMs);
  const since = (at: number | null | undefined) => (at == null ? null : formatElapsed(now - at));
  return {
    inState: since(glance.status_since_ms),
    session: since(glance.session_started_at_ms ?? glance.created_at_ms),
    lastActivity: since(glance.transcript?.updated_at_ms),
  };
}

/**
 * The timing sentence under the headline: `Running for 12m · session 2h ·
 * last activity 8s ago`. `statusLabel` is the shared status vocabulary's label.
 */
export function timingLine(statusLabel: string, timings: GlanceTimings): string {
  return [
    timings.inState ? `${statusLabel} for ${timings.inState}` : statusLabel,
    timings.session ? `session ${timings.session}` : null,
    timings.lastActivity ? `last activity ${timings.lastActivity} ago` : null,
  ].filter(Boolean).join(' · ');
}

/** `14 tool calls since the prompt`, or null when there were none. */
export function toolCountLine(glance: NodeGlance): string | null {
  const count = glance.transcript?.tool_calls_since_prompt ?? 0;
  if (count === 0) return null;
  return `${count} tool call${count === 1 ? '' : 's'} since the prompt`;
}
