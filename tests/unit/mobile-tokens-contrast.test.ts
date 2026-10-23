/** Mobile text contrast across the surfaces used by the SPA. */

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

function readMobileVar(name: string): string {
  const re = new RegExp(`--${name}\\s*:\\s*([^;]+);`);
  const m = re.exec(MOBILE_CSS);
  if (!m) throw new Error(`Missing mobile token --${name}`);
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

  it('--on-accent clears 4.5:1 on --accent and --accent-dim', () => {
    const onAccent = readMobileVar('on-accent');
    expect(contrastRatio(onAccent, readMobileVar('accent'))).toBeGreaterThanOrEqual(4.5);
    expect(contrastRatio(onAccent, readMobileVar('accent-dim'))).toBeGreaterThanOrEqual(4.5);
  });

  it('the text hierarchy stays monotonic on --bg', () => {
    const bg = readMobileVar('bg');
    const primary = contrastRatio(readMobileVar('text'), bg);
    const secondary = contrastRatio(readMobileVar('text-dim'), bg);
    const muted = contrastRatio(readMobileVar('text-faint'), bg);
    expect(primary).toBeGreaterThan(secondary);
    expect(secondary).toBeGreaterThan(muted);
  });
});
