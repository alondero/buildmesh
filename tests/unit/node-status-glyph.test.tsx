// The unified node glyph: one SVG that carries the node's status (the circle)
// and Circuit's state (the orbit ring). The orbit shapes are chosen so every
// Circuit state reads without colour and without motion.
import { describe, it, expect, vi, afterEach } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { fireEvent, render, screen } from '@testing-library/react';
import { NodeStatusGlyph } from '../../src/components/shared/NodeStatusGlyph';
import { STATUS_CONFIG } from '../../src/lib/status';
import {
  glyphLabel,
  glyphTitle,
  orbitFor,
} from '../../src/lib/nodeStatusGlyph';
import type { CircuitNodePresentation } from '../../src/lib/circuitNodePresentation';

const APP_CSS = readFileSync(resolve(__dirname, '../../src/App.css'), 'utf8');

const ACTIVE: CircuitNodePresentation = { phase: 'active', tone: 'automation', label: 'Circuit active', detail: 'Circuit is driving this Agent Node.' };
const WAITING: CircuitNodePresentation = { phase: 'waiting', tone: 'warning', label: 'Circuit waiting', detail: 'Circuit is paused.' };
const NEEDS_ATTENTION: CircuitNodePresentation = { phase: 'waiting', tone: 'error', label: 'Circuit needs attention', detail: 'Circuit reported a failure or an unknown state and needs attention.' };
const DONE: CircuitNodePresentation = { phase: 'done', tone: 'success', label: 'Circuit done', detail: 'Circuit finished; this Agent Node remains available for review.' };

const ACTION_LABEL = 'Open this Circuit run in the Circuits Probe.';

const base = {
  shape: 'solid' as const,
  colorClass: 'status-running',
  statusLabel: 'Running',
  statusTitle: 'Running',
  circuit: null,
};

afterEach(() => vi.restoreAllMocks());

describe('status circle shapes', () => {

  it('never lets two statuses share both shape and colour, so a colour-blind reader can always tell them apart', () => {
    // The actual collision set in STATUS_CONFIG today:
    //   pending|spawning — the same "starting" state mirrored from two code
    //     paths; same dashed ring, same muted colour, and the card title bar
    //     always paints "Starting…" as a label, so the sidebar reads them
    //     identically and that is by design.
    //   completed|ready — pre-existing ✓/green grouping (PR opened and a
    //     node that has just yielded); both render the green ringed dot.
    //     This is the same colourblind risk the old text dots had, but the
    //     card title bar always paints the label as text, so the sidebar
    //     collision is bounded to this one pair.
    // Everything else must differ in either shape or colour. Adding a
    // status that collides with a non-allowed status is forced to break
    // this test and explain itself.
    const ALLOWED_PAIRS = new Set([
      'pending|spawning',
      'completed|ready',
    ]);
    // `color` carries the pulse class on some statuses; only the first
    // token is the actual colour.
    const colourKey = (c: { color: string }) => c.color.trim().split(/\s+/)[0];
    const seen = new Map<string, string>();
    for (const [id, c] of Object.entries(STATUS_CONFIG)) {
      const key = `${c.glyph}|${colourKey(c)}`;
      const other = seen.get(key);
      const pairKey = [id, other].sort().join('|');
      if (other && !ALLOWED_PAIRS.has(pairKey)) throw new Error(
        `${id} and ${other} share shape="${c.glyph}" and colour="${colourKey(c)}"; the sidebar would render them identically. Change one.`,
      );
      seen.set(key, id);
    }
  });
});

describe('orbitFor', () => {
  it.each([
    ['active', ACTIVE, 'comet', 'text-accent-violet'],
    ['waiting', WAITING, 'half', 'text-accent-amber'],
    ['needs attention', NEEDS_ATTENTION, 'dashed', 'text-status-error'],
    ['done', DONE, 'closed', 'text-accent-green'],
  ] as const)('draws %s as the %s ring', (_name, presentation, shape, colorClass) => {
    expect(orbitFor(presentation)).toEqual({ shape, colorClass });
  });

  it('draws no ring for an unpiloted node', () => {
    expect(orbitFor(null)).toBeNull();
  });

  it('keeps the comet silhouette exclusive to the active state', () => {
    const shapes = [WAITING, NEEDS_ATTENTION, DONE].map(p => orbitFor(p)?.shape);
    expect(shapes).not.toContain('comet');
    expect(new Set(shapes).size).toBe(3);
  });
});

describe('accessible name and tooltip', () => {
  it('names the status alone when the node is not piloted', () => {
    expect(glyphLabel('Running', null)).toBe('Running');
  });

  it('adds the Circuit state so one glyph reads as one fact', () => {
    expect(glyphLabel('Needs attention', WAITING)).toBe('Needs attention. Circuit waiting');
  });

  it('does not stack a full stop on a status label that already ends in punctuation', () => {
    // "Starting…" is a real status label.
    expect(glyphLabel('Starting…', WAITING)).toBe('Starting… Circuit waiting');
    expect(glyphTitle('Starting…', WAITING)).toBe('Starting… Circuit is paused.');
  });

  it('joins status, Circuit detail and the action without doubled full stops', () => {
    expect(glyphTitle('Running', null)).toBe('Running');
    expect(glyphTitle('Running', ACTIVE)).toBe('Running. Circuit is driving this Agent Node.');
    // A status tooltip that already ends in a sentence (signal-health note).
    expect(glyphTitle('Running. Status reporting is not confirmed yet.', ACTIVE, ACTION_LABEL))
      .toBe(`Running. Status reporting is not confirmed yet. Circuit is driving this Agent Node. ${ACTION_LABEL}`);
  });
});

describe('NodeStatusGlyph', () => {
  it('is one labelled image for an unpiloted node, with no orbit', () => {
    const { container } = render(<NodeStatusGlyph {...base} />);
    const glyph = screen.getByRole('img', { name: 'Running' });
    expect(glyph.getAttribute('title')).toBe('Running');
    expect(glyph.getAttribute('data-shape')).toBe('solid');
    expect(container.querySelector('[data-orbit]')).toBeNull();
    expect(screen.queryByRole('button')).toBeNull();
    expect(container.querySelectorAll('svg')).toHaveLength(1);
  });

  it.each([
    ['active', ACTIVE, 'comet'],
    ['waiting', WAITING, 'half'],
    ['needs attention', NEEDS_ATTENTION, 'dashed'],
    ['done', DONE, 'closed'],
  ] as const)('draws the %s Circuit state as the %s orbit and says so in its name', (_n, presentation, orbit) => {
    const { container } = render(<NodeStatusGlyph {...base} circuit={presentation} />);
    expect(container.querySelector('[data-orbit]')?.getAttribute('data-orbit')).toBe(orbit);
    expect(screen.getByRole('img', { name: `Running. ${presentation.label}` })).toBeTruthy();
    expect(screen.getByTestId('node-status-glyph').getAttribute('title')).toBe(`Running. ${presentation.detail}`);
  });

  describe('when it can open the Circuit run', () => {
    it('is a button named for status, Circuit state and action, with a 24px target', () => {
      const onActivate = vi.fn();
      render(<NodeStatusGlyph {...base} circuit={ACTIVE} action={{ label: ACTION_LABEL, onActivate }} />);
      const button = screen.getByRole('button', { name: `Running. Circuit active. ${ACTION_LABEL}` });
      expect(button.getAttribute('title')).toBe(`Running. ${ACTIVE.detail} ${ACTION_LABEL}`);
      expect(button.className).toContain('h-6 w-6');
      expect(button.querySelector('[data-orbit="comet"]')).toBeTruthy();
      fireEvent.click(button);
      expect(onActivate).toHaveBeenCalledOnce();
    });

    it('seals click, pointer-down, and double-click so ancestors never see them', () => {
      // The glyph sits inside the card's drag handle, its double-click target,
      // and its select-and-focus-terminal click handler. A stray bubbling event
      // would start a drag, maximize the card, or pull focus back to the terminal
      // the moment the Circuits Probe opened.
      const onActivate = vi.fn();
      const onAncestorClick = vi.fn();
      const onAncestorPointerDown = vi.fn();
      const onAncestorDoubleClick = vi.fn();
      render(
        <div onClick={onAncestorClick} onPointerDown={onAncestorPointerDown} onDoubleClick={onAncestorDoubleClick}>
          <NodeStatusGlyph {...base} circuit={ACTIVE} action={{ label: ACTION_LABEL, onActivate }} />
        </div>,
      );
      const button = screen.getByTestId('node-status-glyph');
      fireEvent.pointerDown(button);
      fireEvent.doubleClick(button);
      fireEvent.click(button);

      expect(onActivate).toHaveBeenCalledOnce();
      expect(onAncestorClick).not.toHaveBeenCalled();
      expect(onAncestorPointerDown).not.toHaveBeenCalled();
      expect(onAncestorDoubleClick).not.toHaveBeenCalled();
    });

    it('stays a static image without an action, even when piloted', () => {
      render(<NodeStatusGlyph {...base} circuit={ACTIVE} />);
      expect(screen.queryByRole('button')).toBeNull();
    });
  });
});

describe('reduced motion', () => {
  it('parks the comet at 12 o\'clock instead of freezing it where the spin stopped', () => {
    // jsdom cannot evaluate a media query, so pin the stylesheet rule: without
    // it the global reduced-motion rule leaves "piloting" as a comet at its
    // 3 o'clock start, which looks like no more than a decoration.
    expect(APP_CSS).toMatch(
      /@media \(prefers-reduced-motion: reduce\)\s*\{[^@]*?\.node-glyph-comet\s*\{[^}]*animation:\s*none[^}]*transform:\s*rotate\(-90deg\)/s,
    );
  });
});
