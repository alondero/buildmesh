import type { CircuitIndicatorPhase, CircuitIndicatorTone, CircuitNodePresentation } from './circuitNodePresentation';

/**
 * The ring drawn around a node's status circle for Circuit's state. Each shape
 * is different from the others in outline, not just colour, and the comet
 * (tapering tail plus star head) belongs to the active state alone, so every
 * state still reads in greyscale and with motion switched off.
 */
export type OrbitShape = 'comet' | 'half' | 'dashed' | 'closed';

export interface OrbitSpec {
  shape: OrbitShape;
  /** Text colour class; the SVG strokes use `currentColor`. */
  colorClass: string;
}

const SHAPE_BY_PHASE: Record<CircuitIndicatorPhase, OrbitShape> = {
  active: 'comet',
  waiting: 'half',
  done: 'closed',
};

const COLOR_BY_TONE: Record<CircuitIndicatorTone, string> = {
  automation: 'text-accent-violet',
  warning: 'text-accent-amber',
  success: 'text-accent-green',
  error: 'text-status-error',
};

export function orbitFor(presentation: CircuitNodePresentation | null): OrbitSpec | null {
  if (!presentation) return null;
  // A Circuit that failed or reported an unknown state is a waiting phase with
  // an error tone; it gets the broken (dashed) ring rather than the held half
  // ring so "paused" and "needs a human" differ by shape.
  const shape = presentation.tone === 'error' ? 'dashed' : SHAPE_BY_PHASE[presentation.phase];
  return { shape, colorClass: COLOR_BY_TONE[presentation.tone] };
}

// "Starting…" is a status label, so an ellipsis closes a sentence too.
const endsSentence = (text: string) => /[.!?…]$/.test(text);

const joinSentences = (head: string, tail: string) => `${head}${endsSentence(head) ? '' : '.'} ${tail}`;

/** One accessible name for the whole glyph: the node's status, then Circuit's. */
export function glyphLabel(statusLabel: string, presentation: CircuitNodePresentation | null): string {
  return presentation ? joinSentences(statusLabel, presentation.label) : statusLabel;
}

/** Tooltip text: status, Circuit detail, and what activating the glyph does. */
export function glyphTitle(
  statusTitle: string,
  presentation: CircuitNodePresentation | null,
  actionLabel?: string,
): string {
  const parts = [statusTitle];
  if (presentation) parts.push(presentation.detail);
  if (actionLabel) parts.push(actionLabel);
  return parts.reduce(joinSentences);
}

/** One full turn of the comet. */
export const COMET_PERIOD_MS = 3200;

/**
 * CSS animation delay that puts a comet on the page-wide clock. Every glyph
 * mounts at a different moment, so unsynchronised spinners would drift apart
 * and a grid of piloted nodes would look noisy; this delay makes the angle a
 * function of `performance.now()` alone.
 */
export function sharedClockDelayMs(mountedAtMs: number, periodMs: number = COMET_PERIOD_MS): number {
  return -(mountedAtMs % periodMs) || 0;
}
