/**
 * Mobile design-token regression — companion to theme-tokens-contrast.test.ts.
 *
 * The mobile SPA (src/mobile/styles.css) doesn't run Tailwind, so its
 * `:root` vars are a hand-mirrored copy of the desktop @theme tokens in
 * src/App.css (mapping table: DESIGN.md §Mobile token mapping). Before the
 * 2026-09 unification pass that mirror had silently drifted to a Material
 * palette with a 3.6:1 muted text colour. This test pins two contracts:
 *
 *   1. WCAG AA: every text tier (--text / --text-dim / --text-faint) clears
 *      4.5:1 on every surface the mobile app renders text onto, and the
 *      hierarchy stays monotonic (--text > --text-dim > --text-faint).
 *   2. Parity: the mobile vars equal the desktop dark-theme tokens they
 *      mirror, so retuning one file without the other fails the gate.
 *
 * Contrast math is re-implemented inline (no third-party a11y dep),
 * matching the desktop companion test.
 */

import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

// ---------- WCAG contrast helpers -----------------------------------------

function srgbChannelToLinear(c: number): number {
  const cs = c / 255;
  return cs <= 0.03928 ? cs / 12.92 : Math.pow((cs + 0.055) / 1.055, 2.4);
}

function relativeLuminance(hex: string): number {
  const m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex);
  if (!m) throw new Error(`Not a 6-digit hex colour: ${hex}`);
  const r = parseInt(m[1], 16);
  const g = parseInt(m[2], 16);
  const b = parseInt(m[3], 16);
  return (
    0.2126 * srgbChannelToLinear(r) +
    0.7152 * srgbChannelToLinear(g) +
    0.0722 * srgbChannelToLinear(b)
  );
}

function contrastRatio(fg: string, bg: string): number {
  const L1 = relativeLuminance(fg);
  const L2 = relativeLuminance(bg);
  const lighter = Math.max(L1, L2);
  const darker = Math.min(L1, L2);
  return (lighter + 0.05) / (darker + 0.05);
}

// ---------- Token extraction ----------------------------------------------

const MOBILE_CSS = readFileSync(
  resolve(__dirname, '../../src/mobile/styles.css'),
  'utf8',
);
const APP_CSS = readFileSync(resolve(__dirname, '../../src/App.css'), 'utf8');

function readMobileVar(name: string): string {
  const re = new RegExp(`--${name}\\s*:\\s*([^;]+);`);
  const m = re.exec(MOBILE_CSS);
  if (!m) throw new Error(`Missing mobile token --${name}`);
  return m[1].trim();
}

function readDesktopToken(name: string): string {
  // The dark-theme value is the FIRST occurrence (the light override block
  // repeats each token name).
  const re = new RegExp(`--color-${name}\\s*:\\s*([^;]+);`);
  const m = re.exec(APP_CSS);
  if (!m) throw new Error(`Missing desktop token --color-${name}`);
  return m[1].trim();
}

const MOBILE_SURFACES = ['bg', 'surface', 'surface-2', 'surface-3'] as const;
const MOBILE_TEXT_TIERS = ['text', 'text-dim', 'text-faint'] as const;

// ---------- Tests ----------------------------------------------------------

describe('mobile text contrast (WCAG AA)', () => {
  for (const tier of MOBILE_TEXT_TIERS) {
    for (const surface of MOBILE_SURFACES) {
      it(`--${tier} clears 4.5:1 on --${surface}`, () => {
        const ratio = contrastRatio(
          readMobileVar(tier),
          readMobileVar(surface),
        );
        expect(ratio).toBeGreaterThanOrEqual(4.5);
      });
    }
  }

  it('the text hierarchy stays monotonic on --bg', () => {
    const bg = readMobileVar('bg');
    const primary = contrastRatio(readMobileVar('text'), bg);
    const secondary = contrastRatio(readMobileVar('text-dim'), bg);
    const muted = contrastRatio(readMobileVar('text-faint'), bg);
    expect(primary).toBeGreaterThan(secondary);
    expect(secondary).toBeGreaterThan(muted);
  });
});

describe('mobile ↔ desktop token parity (DESIGN.md mapping table)', () => {
  // [mobile var, desktop --color-* token]
  const pairs: Array<[string, string]> = [
    ['bg', 'bg-base'],
    ['surface', 'bg-surface'],
    ['surface-2', 'bg-card'],
    ['surface-3', 'bg-card-hover'],
    ['text', 'text-primary'],
    ['text-dim', 'text-secondary'],
    ['text-faint', 'text-muted'],
    ['on-accent', 'text-inverse'],
    ['accent', 'accent-cyan'],
    ['accent-dim', 'accent-blue'],
    ['green', 'accent-green'],
    ['red', 'accent-red'],
    ['amber', 'accent-amber'],
    ['violet', 'accent-violet'],
  ];

  for (const [mobileVar, desktopToken] of pairs) {
    it(`--${mobileVar} equals --color-${desktopToken}`, () => {
      expect(readMobileVar(mobileVar).toLowerCase()).toBe(
        readDesktopToken(desktopToken).toLowerCase(),
      );
    });
  }
});
