import type { AutopilotNodePresentation } from '../../lib/autopilotNodePresentation';

interface AutopilotNodeIndicatorProps {
  presentation: AutopilotNodePresentation | null;
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

  const color = presentation.tone === 'automation'
    ? 'text-accent-violet'
    : presentation.tone === 'warning'
      ? 'text-accent-amber'
      : presentation.tone === 'success'
        ? 'text-accent-green'
        : 'text-status-error';

  return (
    <span
      data-testid="autopilot-indicator"
      role="img"
      aria-label={presentation.label}
      title={presentation.detail}
      className={`inline-flex h-3.5 w-3.5 items-center justify-center ${color}`}
    >
      {presentation.phase === 'active' && (
        <svg aria-hidden="true" className="h-3.5 w-3.5 motion-safe:animate-pulse motion-reduce:animate-none" viewBox="0 0 16 16" fill="none">
          <path d="M8 1.5 9.5 6.5 14.5 8 9.5 9.5 8 14.5 6.5 9.5 1.5 8 6.5 6.5 8 1.5Z" fill="currentColor" />
        </svg>
      )}
      {presentation.phase === 'waiting' && presentation.tone !== 'error' && (
        <svg aria-hidden="true" className="h-3.5 w-3.5" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.75" strokeLinecap="round">
          <circle cx="8" cy="8" r="6" />
          <path d="M6.25 5.5v5M9.75 5.5v5" />
        </svg>
      )}
      {presentation.phase === 'waiting' && presentation.tone === 'error' && (
        <svg aria-hidden="true" className="h-3.5 w-3.5" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.75" strokeLinecap="round">
          <circle cx="8" cy="8" r="6" />
          <path d="M8 4.75v4.25M8 11.25v.1" />
        </svg>
      )}
      {presentation.phase === 'done' && (
        <svg aria-hidden="true" className="h-3.5 w-3.5" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.75" strokeLinecap="round" strokeLinejoin="round">
          <circle cx="8" cy="8" r="6" />
          <path d="m5.25 8 1.75 1.75 3.75-4" />
        </svg>
      )}
    </span>
  );
}
