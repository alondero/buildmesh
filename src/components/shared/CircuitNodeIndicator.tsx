import type { CircuitIndicatorPhase, CircuitIndicatorTone } from '../../lib/circuitNodePresentation';

interface CircuitIndicatorGlyphProps {
  phase: CircuitIndicatorPhase;
  tone: CircuitIndicatorTone;
  className?: string;
}

/**
 * The Pilot-light shape for a Circuit presentation, drawn by the header's
 * attention chip. A node's own Circuit state is shown by the orbit ring of
 * `NodeStatusGlyph`, not by this glyph.
 */
export function CircuitIndicatorGlyph({ phase, tone, className = 'h-3.5 w-3.5' }: CircuitIndicatorGlyphProps) {
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
