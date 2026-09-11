import type { AutopilotIndicatorPhase, AutopilotIndicatorTone, AutopilotNodePresentation } from '../../lib/autopilotNodePresentation';

interface AutopilotNodeIndicatorProps {
  presentation: AutopilotNodePresentation | null;
}

interface AutopilotIndicatorGlyphProps {
  phase: AutopilotIndicatorPhase;
  tone: AutopilotIndicatorTone;
  className?: string;
}

const TONE_COLORS: Record<AutopilotIndicatorTone, string> = {
  automation: 'text-accent-violet',
  warning: 'text-accent-amber',
  success: 'text-accent-green',
  error: 'text-status-error',
};

/**
 * The Pilot-light shape for an Autopilot presentation. Shared by the fixed
 * identity-cell indicator and the header's outcome chip so both draw the same
 * active/waiting/done vocabulary from one place (see
 * `docs/specs/autopilot-node-indicators.md`).
 */
export function AutopilotIndicatorGlyph({ phase, tone, className = 'h-3.5 w-3.5' }: AutopilotIndicatorGlyphProps) {
  if (phase === 'active') {
    return (
      <svg aria-hidden="true" className={`${className} motion-safe:animate-pulse motion-reduce:animate-none`} viewBox="0 0 16 16" fill="none">
        <path d="M8 1.5 9.5 6.5 14.5 8 9.5 9.5 8 14.5 6.5 9.5 1.5 8 6.5 6.5 8 1.5Z" fill="currentColor" />
      </svg>
    );
  }
  if (phase === 'waiting' && tone === 'error') {
    return (
      <svg aria-hidden="true" className={className} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.75" strokeLinecap="round">
        <circle cx="8" cy="8" r="6" />
        <path d="M8 4.75v4.25M8 11.25v.1" />
      </svg>
    );
  }
  if (phase === 'waiting') {
    return (
      <svg aria-hidden="true" className={className} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.75" strokeLinecap="round">
        <circle cx="8" cy="8" r="6" />
        <path d="M6.25 5.5v5M9.75 5.5v5" />
      </svg>
    );
  }
  return (
    <svg aria-hidden="true" className={className} viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.75" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="8" cy="8" r="6" />
      <path d="m5.25 8 1.75 1.75 3.75-4" />
    </svg>
  );
}

/** Reserves the shared 14px identity column even when the light is absent. */
export function AutopilotNodeIndicatorCell({ presentation }: AutopilotNodeIndicatorProps) {
  return (
    <span data-testid="autopilot-indicator-cell" className="inline-flex h-3.5 w-3.5 shrink-0 items-center justify-center">
      <AutopilotNodeIndicator presentation={presentation} />
    </span>
  );
}

/** The compact, shape-plus-label indicator used by both node identity rows. */
export function AutopilotNodeIndicator({ presentation }: AutopilotNodeIndicatorProps) {
  if (!presentation) return null;

  return (
    <span
      data-testid="autopilot-indicator"
      role="img"
      aria-label={presentation.label}
      title={presentation.detail}
      className={`inline-flex h-3.5 w-3.5 items-center justify-center ${TONE_COLORS[presentation.tone]}`}
    >
      <AutopilotIndicatorGlyph phase={presentation.phase} tone={presentation.tone} />
    </span>
  );
}
