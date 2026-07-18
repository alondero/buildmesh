import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

/**
 * Light-theme contrast regression — issue #734.
 *
 * The dark-theme test (theme-tokens-contrast.test.ts, #732) pins the AA
 * contract for the @theme tokens. This test does the same for the
 * `[data-theme="light"] { ... }` override block: text-muted must clear
 * 4.5:1 on every surface the design system actually renders text onto,
 * the text-token hierarchy must stay monotonic, and a few load-bearing
 * token values are pinned exactly so a future "let's soften the muted"
 * revert is caught at the test boundary, not by visual review.
 *
 * The override block lives OUTSIDE @theme (deliberately — Tailwind v4
 * only generates utilities for tokens declared IN @theme, and adding
 * new names would require regenerating the utility layer). So this
 * regex scopes its token reads to the [data-theme="light"] block.
 */

// ---------- WCAG contrast helpers (mirror theme-tokens-contrast.test.ts) ----

function srgbChannelToLinear(c: number): number {
  const cs = c / 255;
  return cs <= 0.03928 ? cs / 12.92 : Math.pow((cs + 0.055) / 1.055, 2.4);
}

function relativeLuminance(hex: string): number {
  // Strip alpha if a 8-digit hex sneaks in; the light palette uses
  // 6-digit tokens only for the contrast-tested values.
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

// ---------- Token extraction (scoped to the [data-theme="light"] block) ----

const APP_CSS = readFileSync(resolve(__dirname, '../../src/App.css'), 'utf8');

// Slice the [data-theme="light"] { ... } block. Using a balanced-brace
// match by walking char-by-char after the opening `{` so the slice
// survives the nested `::-webkit-scrollbar-*` selectors that were
// added in the same block.
function readLightBlock(): string {
  const start = APP_CSS.indexOf('[data-theme="light"]');
  if (start === -1) throw new Error('[data-theme="light"] block not found in src/App.css');
  const openBrace = APP_CSS.indexOf('{', start);
  if (openBrace === -1) throw new Error('[data-theme="light"] block has no opening brace');
  let depth = 1;
  let i = openBrace + 1;
  while (i < APP_CSS.length && depth > 0) {
    const ch = APP_CSS[i];
    if (ch === '{') depth++;
    else if (ch === '}') depth--;
    i++;
  }
  return APP_CSS.slice(start, i);
}

const LIGHT_BLOCK = readLightBlock();

function readLightToken(name: string): string {
  // Picks the FIRST occurrence inside the light block — tokens are
  // declared once per block. Matches `--color-bg-base: #fafafa;` (and
  // tolerates surrounding whitespace).
  const re = new RegExp(`--color-${name}\\s*:\\s*([^;]+);`);
  const m = re.exec(LIGHT_BLOCK);
  if (!m) throw new Error(`Token --color-${name} not found in [data-theme="light"] block`);
  return m[1].trim();
}

// Pinned values — a future "let's soften the muted" PR fails the gate
// here, not at visual review.
const EXPECTED_LIGHT_TEXT_MUTED = '#5b6471';
const LIGHT_BG_BASE = '#fafafa';
const LIGHT_BG_SURFACE = '#ffffff';
const LIGHT_BG_CARD = '#f5f5f7';
const AA_BODY_THRESHOLD = 4.5;

// ---------- Tests -----------------------------------------------------------

describe('light theme text-token contrast (#734)', () => {
  it('declares --color-text-muted as the AA-passing palette grey', () => {
    expect(readLightToken('text-muted')).toBe(EXPECTED_LIGHT_TEXT_MUTED);
  });

  it('--color-text-muted clears WCAG AA (4.5:1) on bg-base', () => {
    // Anchors the whole point of the override: the new muted token
    // must read on the new light surface. #5b6471 vs #fafafa lands at
    // ~4.7:1, just clearing AA. A future brighten of muted to ~#6b7481
    // would fail this gate.
    const ratio = contrastRatio(EXPECTED_LIGHT_TEXT_MUTED, LIGHT_BG_BASE);
    expect(ratio).toBeGreaterThanOrEqual(AA_BODY_THRESHOLD);
  });

  it('--color-text-muted clears WCAG AA (4.5:1) on bg-surface', () => {
    // bg-surface = #ffffff (the panel background). The contrast here
    // is the tightest of the three (pure white has the highest
    // luminance) — pinning the contract here catches a future
    // darken of text-muted that "still works on bg-base".
    const ratio = contrastRatio(EXPECTED_LIGHT_TEXT_MUTED, LIGHT_BG_SURFACE);
    expect(ratio).toBeGreaterThanOrEqual(AA_BODY_THRESHOLD);
  });

  it('--color-text-muted clears WCAG AA (4.5:1) on bg-card', () => {
    // bg-card = #f5f5f7 is the probe-tab / mesh-panel surface. Just
    // clearing AA is fine; the margin is tighter than bg-base.
    const ratio = contrastRatio(EXPECTED_LIGHT_TEXT_MUTED, LIGHT_BG_CARD);
    expect(ratio).toBeGreaterThanOrEqual(AA_BODY_THRESHOLD);
  });

  it('--color-text-muted stays subordinate to --color-text-secondary', () => {
    // Hierarchy guard: text-muted must be visually LESS prominent than
    // text-secondary, otherwise the two tokens collapse into one and the
    // design system loses a tier (the dark-theme equivalent of this test
    // is theme-tokens-contrast.test.ts).
    const muted = contrastRatio(readLightToken('text-muted'), LIGHT_BG_BASE);
    const secondary = contrastRatio(readLightToken('text-secondary'), LIGHT_BG_BASE);
    expect(muted).toBeLessThan(secondary);
  });

  it('text-token ordering on bg-base is monotonic (primary > secondary > muted > base)', () => {
    // Same shape as the dark-theme test — a "let's brighten text-muted"
    // change that overshoots text-secondary would flatten the hierarchy.
    const primary = contrastRatio(readLightToken('text-primary'), LIGHT_BG_BASE);
    const secondary = contrastRatio(readLightToken('text-secondary'), LIGHT_BG_BASE);
    const muted = contrastRatio(readLightToken('text-muted'), LIGHT_BG_BASE);
    expect(primary).toBeGreaterThan(secondary);
    expect(secondary).toBeGreaterThan(muted);
    expect(muted).toBeGreaterThan(contrastRatio(LIGHT_BG_BASE, LIGHT_BG_BASE)); // == 1
  });
});

// ---------- Token-existence guards -----------------------------------------
//
// Tailwind v4 only emits a `bg-bg-card` / `text-accent-cyan` utility when
// the matching --color-* token is declared in @theme. The override block
// re-points EXISTING tokens (it doesn't add new names) — without the
// corresponding entry in @theme, the utility would silently render
// nothing. This guard makes the override <-> @theme coverage explicit.

describe('light theme token coverage (#734)', () => {
  // Tokens the override block re-points. If you add a new colour token
  // to @theme and the UI starts reading it in light mode, add it here too.
  const LIGHT_TOKENS = [
    'bg-base',
    'bg-surface',
    'bg-overlay',
    'bg-card',
    'bg-card-hover',
    'bg-input',
    'bg-selection',
    'bg-highlight',
    'text-primary',
    'text-secondary',
    'text-muted',
    'text-inverse',
    'accent-cyan',
    'accent-cyan-dim',
    'accent-cyan-glow',
    'accent-blue',
    'accent-blue-dim',
    'accent-violet',
    'accent-violet-dim',
    'accent-green',
    'accent-red',
    'accent-amber',
    'status-success',
    'status-success-bg',
    'status-warning',
    'status-warning-bg',
    'status-error',
    'status-error-bg',
    'status-idle',
    'status-idle-bg',
    'status-running',
    'status-running-bg',
    'status-blocked',
    'border-subtle',
    'border-default',
    'border-strong',
    'border-active',
  ];

  it.each(LIGHT_TOKENS)(
    'overrides --color-%s (so utilities flip at the cascade, not per-component)',
    (name) => {
      const re = new RegExp(`--color-${name}\\s*:`);
      expect(LIGHT_BLOCK, `[data-theme="light"] must override --color-${name}`).toMatch(re);
    },
  );

  it('does NOT introduce tokens absent from @theme (Tailwind v4 utility layer depends on @theme coverage)', () => {
    // Inverse check: the override block must only re-point tokens that
    // @theme already knows about. Adding a fresh --color-* here would
    // have no effect at runtime (Tailwind wouldn't emit a utility for it)
    // and would silently drift the override from the utility layer.
    const overrideTokenNames = new Set<string>();
    const re = /--color-([a-z0-9-]+)\s*:/g;
    let m: RegExpExecArray | null;
    while ((m = re.exec(LIGHT_BLOCK)) !== null) overrideTokenNames.add(m[1]);

    const themeTokenNames = new Set<string>();
    const themeBlockMatch = /@theme\s*{([\s\S]*?)^\}/m.exec(APP_CSS);
    if (!themeBlockMatch) throw new Error('@theme block not found in src/App.css');
    const themeBlock = themeBlockMatch[1];
    const themeRe = /--color-([a-z0-9-]+)\s*:/g;
    while ((m = themeRe.exec(themeBlock)) !== null) themeTokenNames.add(m[1]);

    const drift = [...overrideTokenNames].filter((n) => !themeTokenNames.has(n));
    expect(drift, `Override tokens missing from @theme: ${drift.join(', ')}`).toEqual([]);
  });
});