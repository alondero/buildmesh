/**
 * fitPanelToBounds — the pure geometry behind every anchored overlay
 * panel (issue #2773).
 *
 * The settings help tip and the Circuit Quick Connect menu both open
 * against an anchor that may sit near an edge of the area the user can
 * see. Before this module they placed themselves with a hard-coded
 * offset, so a trigger near an edge pushed the panel out of sight.
 *
 * These assertions are geometry, not class strings: they state where the
 * panel has to end up and how tall it is allowed to be, which is the part
 * of the behaviour a user actually notices.
 */
import { describe, it, expect } from 'vitest';
import { fitPanelToBounds, OVERLAY_MARGIN } from '../../src/lib/overlayLayout';

const BOUNDS = { width: 900, height: 600 };
const PANEL = { width: 288, height: 180 };

/**
 * The shape an anchored panel is measured with: the trigger sits at
 * `anchorLeft`/`anchorTop`, the panel opens below it with a 4px gap, and
 * the alternate placement opens above the trigger with its bottom edge
 * 4px above the trigger's top.
 */
function fitAt(anchorLeft: number, anchorTop: number, size = PANEL, bounds = BOUNDS) {
  return fitPanelToBounds({
    size,
    preferred: { left: anchorLeft, top: anchorTop + 4 },
    alternate: { left: anchorLeft + 24 - size.width, bottom: anchorTop - 4 },
    bounds,
  });
}

describe('fitPanelToBounds', () => {
  it('opens below the anchor when the panel fits there', () => {
    const fit = fitAt(100, 200);
    expect(fit.side).toBe('preferred');
    expect(fit.top).toBe(204);
    expect(fit.height).toBe(PANEL.height);
    expect(fit.left).toBe(100);
  });

  it('flips above the anchor when the panel does not fit below', () => {
    // The anchor is 500px down a 600px-tall area: only 88px is left
    // below it, so the 180px panel opens upwards instead.
    const fit = fitAt(100, 500);
    expect(fit.side).toBe('alternate');
    expect(fit.top).toBe(496 - PANEL.height);
    expect(fit.height).toBe(PANEL.height);
  });

  it('bounds the height on the roomier side when the panel fits on neither', () => {
    // The anchor sits 524px down a 600px-tall area: 64px of room below
    // it and 512px above it. A 600px panel fits on neither side, so it
    // opens above and its height is capped to the room there.
    const fit = fitAt(100, 524, { width: 288, height: 600 });
    expect(fit.side).toBe('alternate');
    expect(fit.maxHeight).toBe(512);
    expect(fit.height).toBe(512);
    expect(fit.top).toBe(520 - 512);
  });

  it('opens on the roomier side below when neither side fits', () => {
    // The mirror case: 488px of room below the anchor, 88px above it.
    const fit = fitAt(100, 100, { width: 288, height: 600 });
    expect(fit.side).toBe('preferred');
    expect(fit.maxHeight).toBe(488);
    expect(fit.height).toBe(488);
    expect(fit.top).toBe(104);
  });

  it('flips to the anchor\'s other edge when the preferred edge overflows', () => {
    // 700 + 288 runs past the 900px area, so the panel aligns with the
    // anchor's right edge (724 - 288 = 436) instead.
    const fit = fitAt(700, 100);
    expect(fit.left).toBe(436);
    expect(fit.left + PANEL.width).toBeLessThanOrEqual(BOUNDS.width - OVERLAY_MARGIN);
  });

  it('clamps into the bounds when neither edge fits', () => {
    const fit = fitAt(850, 100, { width: 400, height: 180 });
    expect(fit.left + 400).toBeLessThanOrEqual(BOUNDS.width - OVERLAY_MARGIN);
    expect(fit.left).toBeGreaterThanOrEqual(OVERLAY_MARGIN);
  });

  it('keeps the panel inside the bounds when the anchor is outside them', () => {
    const fit = fitAt(100, 900);
    expect(fit.top).toBeGreaterThanOrEqual(OVERLAY_MARGIN);
    expect(fit.top + fit.height).toBeLessThanOrEqual(BOUNDS.height - OVERLAY_MARGIN);
  });

  it('keeps every panel inside the bounds for a sweep of anchor positions', () => {
    for (let x = 0; x <= BOUNDS.width; x += 20) {
      for (let y = 0; y <= BOUNDS.height; y += 20) {
        const fit = fitAt(x, y);
        const label = `anchor ${x},${y}`;
        expect(fit.left, label).toBeGreaterThanOrEqual(OVERLAY_MARGIN);
        expect(fit.left + PANEL.width, label).toBeLessThanOrEqual(BOUNDS.width - OVERLAY_MARGIN);
        expect(fit.top, label).toBeGreaterThanOrEqual(OVERLAY_MARGIN);
        expect(fit.top + fit.height, label).toBeLessThanOrEqual(BOUNDS.height - OVERLAY_MARGIN);
        expect(fit.height, label).toBeLessThanOrEqual(fit.maxHeight);
      }
    }
  });

  it('keeps a tall panel inside a short area by bounding its height', () => {
    const bounds = { width: 320, height: 240 };
    const fit = fitPanelToBounds({
      size: { width: 288, height: 600 },
      preferred: { left: 10, top: 20 },
      alternate: { left: 10, bottom: 16 },
      bounds,
    });
    expect(fit.height).toBeLessThanOrEqual(bounds.height - OVERLAY_MARGIN * 2);
    expect(fit.top + fit.height).toBeLessThanOrEqual(bounds.height - OVERLAY_MARGIN);
  });
});
