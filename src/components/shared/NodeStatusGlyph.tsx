import { useState, type SyntheticEvent } from 'react';
import type { CircuitNodePresentation } from '../../lib/circuitNodePresentation';
import type { StatusGlyphShape } from '../../lib/status';
import {
  COMET_PERIOD_MS,
  glyphLabel,
  glyphTitle,
  orbitFor,
  sharedClockDelayMs,
  type OrbitSpec,
} from '../../lib/nodeStatusGlyph';

interface NodeStatusGlyphAction {
  /// Sentence describing what activation does, appended to the accessible name
  /// and tooltip (e.g. "Open this Circuit run in the Circuits Probe.").
  label: string;
  onActivate: () => void;
}

interface NodeStatusGlyphProps {
  shape: StatusGlyphShape;
  /// Text colour (and any pulse) classes for the circle only. The orbit takes
  /// its colour from the Circuit state, and must not pulse with the circle.
  colorClass: string;
  statusLabel: string;
  statusTitle: string;
  circuit: CircuitNodePresentation | null;
  /// When set, the glyph renders as a button instead of a static image.
  action?: NodeStatusGlyphAction;
}

/// Everything is drawn in a 24-unit box with the circle at the centre; the
/// orbit sits on a radius that leaves a gap to the circle at 20px.
const ORBIT_RADIUS = 8.4;

/// Activation must stay local: the glyph sits inside a drag handle, a
/// double-click target, and the card's select-and-focus-terminal click handler,
/// so none of those may see the event. `SyntheticEvent` is the common base of
/// the pointer, mouse, and double-click events these handlers receive.
function stopPropagation(event: SyntheticEvent) {
  event.stopPropagation();
}

function sparklePath(cx: number, cy: number, r: number): string {
  const k = r * 0.26;
  return `M${cx} ${cy - r}L${cx + k} ${cy - k}L${cx + r} ${cy}L${cx + k} ${cy + k}L${cx} ${cy + r}L${cx - k} ${cy + k}L${cx - r} ${cy}L${cx - k} ${cy - k}Z`;
}

/// Angles run clockwise from 3 o'clock; `pathLength=360` lets dash lengths be
/// degrees. The arc is drawn from 0 and rotated into place.
function Arc({ start, length, width, opacity = 1 }: { start: number; length: number; width: number; opacity?: number }) {
  return (
    <g transform={`rotate(${start} 12 12)`}>
      <circle
        cx="12" cy="12" r={ORBIT_RADIUS} pathLength="360" fill="none" stroke="currentColor"
        strokeWidth={width} strokeLinecap="round" strokeDasharray={`${length} 720`} opacity={opacity}
      />
    </g>
  );
}

function Track() {
  return <circle className="node-glyph-track" cx="12" cy="12" r={ORBIT_RADIUS} fill="none" stroke="currentColor" strokeWidth="1" />;
}

function Comet() {
  // Read once at mount: a changing delay would make the running animation jump.
  const [mountedAt] = useState(() => performance.now());
  return (
    <>
      <Track />
      <g
        className="node-glyph-comet"
        style={{ animationDuration: `${COMET_PERIOD_MS}ms`, animationDelay: `${sharedClockDelayMs(mountedAt)}ms` }}
      >
        <Arc start={-120} length={120} width={1.3} opacity={0.22} />
        <Arc start={-70} length={70} width={1.5} opacity={0.5} />
        <Arc start={-26} length={26} width={1.7} />
        <path d={sparklePath(12 + ORBIT_RADIUS, 12, 2.7)} fill="currentColor" />
      </g>
    </>
  );
}

function Orbit({ spec }: { spec: OrbitSpec }) {
  return (
    <g data-orbit={spec.shape} className={spec.colorClass}>
      {spec.shape === 'comet' && <Comet />}
      {spec.shape === 'half' && (
        <>
          <Track />
          <Arc start={180} length={180} width={1.7} />
        </>
      )}
      {spec.shape === 'dashed' && (
        <circle
          cx="12" cy="12" r={ORBIT_RADIUS} pathLength="360" fill="none" stroke="currentColor"
          strokeWidth="1.6" strokeDasharray="18 12"
        />
      )}
      {spec.shape === 'closed' && (
        <circle cx="12" cy="12" r={ORBIT_RADIUS} fill="none" stroke="currentColor" strokeWidth="1.6" opacity="0.95" />
      )}
    </g>
  );
}

function Core({ shape, className }: { shape: StatusGlyphShape; className: string }) {
  const ring = (width: number, extra: { strokeDasharray?: string } = {}) => (
    <circle cx="12" cy="12" r="3.7" fill="none" stroke="currentColor" strokeWidth={width} {...extra} />
  );
  return (
    <g className={className}>
      {shape === 'solid' && <circle cx="12" cy="12" r="4.5" fill="currentColor" />}
      {shape === 'ring' && ring(1.6)}
      {shape === 'dashed' && ring(1.4, { strokeDasharray: '2.2 1.9' })}
      {shape === 'slash' && (
        <>
          {ring(1.6)}
          <path d="M9.2 14.8 14.8 9.2" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
        </>
      )}
      {shape === 'half' && (
        <>
          {ring(1.4)}
          <path d="M12 8.3A3.7 3.7 0 0 0 12 15.7Z" fill="currentColor" />
        </>
      )}
      {shape === 'target' && (
        <>
          <circle cx="12" cy="12" r="3.9" fill="none" stroke="currentColor" strokeWidth="1.4" />
          <circle cx="12" cy="12" r="1.8" fill="currentColor" />
        </>
      )}
      {shape === 'cross' && (
        <path d="M9.2 9.2 14.8 14.8 M14.8 9.2 9.2 14.8" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
      )}
      {shape === 'thin' && ring(1.2)}
    </g>
  );
}

/**
 * One glyph for a node: the circle says what the node is doing, the ring
 * around it says what Circuit is doing. Both are one mark with one accessible
 * name, so a screen reader hears a single fact rather than two fragments.
 */
export function NodeStatusGlyph({ shape, colorClass, statusLabel, statusTitle, circuit, action }: NodeStatusGlyphProps) {
  const orbit = orbitFor(circuit);
  const label = glyphLabel(statusLabel, circuit);
  const svg = (
    <svg aria-hidden="true" viewBox="0 0 24 24" className="block h-5 w-5 shrink-0">
      {orbit && <Orbit spec={orbit} />}
      <Core shape={shape} className={colorClass} />
    </svg>
  );

  if (action) {
    return (
      <button
        type="button"
        data-testid="node-status-glyph"
        data-shape={shape}
        onClick={(event) => {
          // Stop the click before the card's own handler selects the member and
          // re-focuses its terminal, which would pull focus off the Circuits
          // Probe the user just asked to open.
          event.stopPropagation();
          action.onActivate();
        }}
        onPointerDown={stopPropagation}
        onDoubleClick={stopPropagation}
        aria-label={`${label}. ${action.label}`}
        title={glyphTitle(statusTitle, circuit, action.label)}
        // 24px target around the 20px mark; the negative margin keeps the layout
        // footprint at 20px so piloted and unpiloted rows stay aligned.
        className="-m-0.5 inline-flex h-6 w-6 shrink-0 items-center justify-center rounded-sm hover:bg-bg-base/70 focus:outline-none focus-visible:ring-1 focus-visible:ring-accent-cyan"
      >
        {svg}
      </button>
    );
  }

  return (
    <span
      role="img"
      data-testid="node-status-glyph"
      data-shape={shape}
      aria-label={label}
      title={glyphTitle(statusTitle, circuit)}
      className="inline-flex h-5 w-5 shrink-0"
    >
      {svg}
    </span>
  );
}
