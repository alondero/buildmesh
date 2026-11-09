/**
 * The Review label's contrast on a COMPOSED node header — issue #2737.
 *
 * The token-level contrast suites (#732 / #734) compare a text token with a
 * *neutral* surface. The Review label does not sit on one: it sits on a node
 * header whose background is the user's Mesh colour at 8% alpha over the
 * card, and it takes a violet wash on hover. That composition is what put the
 * label at 3.94:1 (the audited Mesh) and as low as 2.93:1 on hover, while the
 * token-only tests still passed.
 *
 * This file therefore measures the *composed* pairs:
 *
 *   header background = Mesh colour @ 8% over `--color-bg-card`
 *   hover background  = `--color-accent-violet` @ 15% over that header
 *   focus background  = the header itself (`:focus-visible` paints an outline
 *                       and changes no background — see src/App.css)
 *
 * The Mesh colour is user-pickable (`MeshColorPicker` accepts any hex), so
 * the worst case is bounded by the corners of the RGB cube rather than by the
 * eight palette swatches: the darkest and lightest possible Mesh colours are
 * both measured here, alongside all eight palette entries and the Mesh the
 * issue audited (#6366f1).
 *
 * The foregrounds are read from the component sources, not hard-coded, so a
 * class change in the components moves this test with it. The baseline
 * (`--color-accent-violet`, the graphic accent) is measured too and asserted
 * to FAIL: a check that the baseline passes would not be a regression test.
 */

import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { MESH_PALETTE } from '../../src/lib/meshColors';

// ---------- Colour maths ----------------------------------------------------

interface Rgb {
  r: number;
  g: number;
  b: number;
}

function srgbChannelToLinear(c: number): number {
  const cs = c / 255;
  return cs <= 0.03928 ? cs / 12.92 : Math.pow((cs + 0.055) / 1.055, 2.4);
}

function relativeLuminance({ r, g, b }: Rgb): number {
  return (
    0.2126 * srgbChannelToLinear(r) +
    0.7152 * srgbChannelToLinear(g) +
    0.0722 * srgbChannelToLinear(b)
  );
}

function contrastRatio(fg: Rgb, bg: Rgb): number {
  const a = relativeLuminance(fg);
  const b = relativeLuminance(bg);
  const lighter = Math.max(a, b);
  const darker = Math.min(a, b);
  return (lighter + 0.05) / (darker + 0.05);
}

function hexToRgb(hex: string): Rgb {
  const m = /^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})$/i.exec(hex.trim());
  if (!m) throw new Error(`Not a 6-digit hex colour: ${hex}`);
  return {
    r: parseInt(m[1], 16),
    g: parseInt(m[2], 16),
    b: parseInt(m[3], 16),
  };
}

/** Composite `fg` at `alpha` over `bg` in sRGB space (what the browser does). */
function over(fg: Rgb, alpha: number, bg: Rgb): Rgb {
  return {
    r: fg.r * alpha + bg.r * (1 - alpha),
    g: fg.g * alpha + bg.g * (1 - alpha),
    b: fg.b * alpha + bg.b * (1 - alpha),
  };
}

function rgbToHex({ r, g, b }: Rgb): string {
  const channel = (v: number) => Math.round(v).toString(16).padStart(2, '0');
  return `#${channel(r)}${channel(g)}${channel(b)}`;
}

// ---------- Sources of truth -----------------------------------------------

const read = (path: string) => readFileSync(resolve(__dirname, '../..', path), 'utf8');

const APP_CSS = read('src/App.css');
const REVIEW_BUTTON = read('src/components/AgentNodeView/AgentReviewButton.tsx');
const GRID_HEADER = read('src/components/AgentNodeView/GridNodeHeader.tsx');

/** The colour tokens of one theme: the dark `@theme` block, or the light override. */
function themeTokens(theme: 'dark' | 'light'): Map<string, string> {
  const scope = theme === 'dark'
    ? /@theme\s*\{([\s\S]*?)\n\}/.exec(APP_CSS)?.[1]
    : /\[data-theme="light"\]\s*\{([\s\S]*?)\n\}/.exec(APP_CSS)?.[1];
  if (!scope) throw new Error(`Could not read the ${theme} token scope from src/App.css`);
  const tokens = new Map<string, string>();
  for (const match of scope.matchAll(/--color-([a-z0-9-]+)\s*:\s*([^;]+);/g)) {
    tokens.set(match[1], match[2].trim());
  }
  return tokens;
}

const DARK = themeTokens('dark');
const LIGHT = themeTokens('light');

function token(tokens: Map<string, string>, name: string): Rgb {
  const value = tokens.get(name);
  if (!value) throw new Error(`Token --color-${name} is not declared for this theme`);
  return hexToRgb(value);
}

/**
 * The text token a component paints with, read from the component's own
 * source — so a class change there moves this measurement with it.
 */
function foregroundToken(source: string, pattern: RegExp, where: string): string {
  const match = pattern.exec(source);
  if (!match) throw new Error(`Could not find ${where} in the component source`);
  return match[1];
}

/** The Review trigger (header label + icon). */
const reviewLabelToken = foregroundToken(
  REVIEW_BUTTON,
  /inline-flex h-6 min-w-6[^`]*?text-(accent-violet(?:-strong)?)/,
  'the Review trigger class',
);
/** The dialog's "View circuit run #N" link. */
const reviewLinkToken = foregroundToken(
  REVIEW_BUTTON,
  /text-xs text-(accent-violet(?:-strong)?) mb-3/,
  'the "View circuit run" link class',
);
/** The dialog's primary action ("Start review" / "Start Circuit"). */
const reviewPrimaryToken = foregroundToken(
  REVIEW_BUTTON,
  /bg-accent-violet\/\d+ text-(accent-violet(?:-strong)?)/,
  'the review dialog primary-action class',
);
/** The automation chip on the node header. */
const automationChipToken = foregroundToken(
  GRID_HEADER,
  /automation: 'bg-accent-violet\/\d+ text-(accent-violet(?:-strong)?)/,
  'the automation chip class',
);
/** The resume glyph on the node header. */
const resumeGlyphToken = foregroundToken(
  GRID_HEADER,
  /grid-resume-button[^>]*text-(accent-violet(?:-strong)?)/,
  'the resume glyph class',
);

// The other places the violet accent is painted as text. Each sits on its own
// surface — the title bar, the omnibar, the sidebar, the Probe — and each
// measured below 4.5:1 with the graphic accent in the dark theme, so they
// carry the text-safe step too.
const COMPANION_PILL = read('src/components/TitleBar/CompanionPill.tsx');
const COMPANION_MODE = read('src/components/CommandOmnibar/CompanionMode.tsx');
const COMPANION_RESPONSE = read('src/components/CommandOmnibar/CompanionResponse.tsx');
const NODE_ITEM = read('src/components/Sidebar/NodeItem.tsx');
const ISSUE_LABELS = read('src/components/Probe/IssueLabels.tsx');
const CIRCUITS_PROBE = read('src/components/Probe/CircuitsProbeTab.tsx');

/** The violet text of the Companion pill in the title bar. */
const companionPillToken = foregroundToken(
  COMPANION_PILL,
  /text-xs text-(accent-violet(?:-strong)?)/,
  'the Companion pill class',
);
/** The violet text of the Companion mode (the "Ask" action). */
const companionAskToken = foregroundToken(
  COMPANION_MODE,
  /className="text-(accent-violet(?:-strong)?) disabled:opacity-50" onClick=\{\(\) => void execute\(prompt\)\}/,
  'the Companion "Ask" button',
);
/** The violet link inside a Companion answer. */
const companionLinkToken = foregroundToken(
  COMPANION_RESPONSE,
  /className="text-(accent-violet(?:-strong)?) underline"/,
  'the Companion answer link class',
);
/** The violet resume glyph in the sidebar row. */
const sidebarResumeToken = foregroundToken(
  NODE_ITEM,
  /hover:text-(accent-violet(?:-strong)?) hover:bg-bg-card-hover/,
  'the sidebar resume glyph class',
);
/** The violet text of a watched issue label chip. */
const issueLabelToken = foregroundToken(
  ISSUE_LABELS,
  /bg-accent-violet\/\d+ text-(accent-violet(?:-strong)?)/,
  'the watched issue-label chip class',
);
/** The violet text of the "Edit Flow" chip in the Circuits Probe. */
const editFlowToken = foregroundToken(
  CIRCUITS_PROBE,
  /bg-accent-violet\/\d+ text-(accent-violet(?:-strong)?) hover:bg-accent-violet/,
  'the "Edit Flow" chip class',
);

/** The header tint: `${meshColor.hex}14` — 8% of the Mesh colour. */
const headerTintAlpha = (() => {
  const match = /backgroundColor: `\$\{meshColor\.hex\}([0-9a-f]{2})`/.exec(GRID_HEADER);
  if (!match) throw new Error('Could not find the Mesh header tint in GridNodeHeader.tsx');
  return parseInt(match[1], 16) / 255;
})();

/** The hover wash on the Review trigger: `hover:bg-accent-violet/15`. */
const reviewHoverAlpha = (() => {
  const match = /hover:bg-accent-violet\/(\d+)/.exec(REVIEW_BUTTON);
  if (!match) throw new Error('Could not find the Review hover wash in AgentReviewButton.tsx');
  return Number(match[1]) / 100;
})();

/** The chip wash and the dialog primary fill: `bg-accent-violet/15`, `/20`. */
const chipWashAlpha = Number(
  /automation: 'bg-accent-violet\/(\d+)/.exec(GRID_HEADER)?.[1] ?? NaN,
) / 100;
const primaryFillAlpha = Number(
  /bg-accent-violet\/(\d+) text-accent-violet/.exec(REVIEW_BUTTON)?.[1] ?? NaN,
) / 100;

// ---------- The compositions ------------------------------------------------

const AA_BODY_TEXT = 4.5;

/** The Mesh colours a header can be tinted with. */
const MESH_COLOURS = [
  ...MESH_PALETTE.map(entry => entry.hex),
  '#6366f1', // the Mesh the issue audited
  '#000000', // the darkest a custom Mesh colour can be
  '#ffffff', // the lightest a custom Mesh colour can be
  ...Array.from({ length: 9 }, (_, r) => Array.from({ length: 9 }, (_, g) => Array.from({ length: 9 }, (_, b) =>
    rgbToHex({
      r: Math.round((r * 255) / 8),
      g: Math.round((g * 255) / 8),
      b: Math.round((b * 255) / 8),
    })))).flat().flat(),
];

interface ThemeMeasurement {
  /** The header background (Mesh tint over the card) for a Mesh colour. */
  header: (mesh: Rgb) => Rgb;
  /** The Review trigger's hover background. */
  hover: (mesh: Rgb) => Rgb;
  /** The automation chip's background. */
  chip: (mesh: Rgb) => Rgb;
  /** The dialog's primary-action background. */
  primary: Rgb;
  /** The dialog's surface (the modal paints `bg-bg-overlay`). */
  overlay: Rgb;
}

function measure(theme: 'dark' | 'light'): ThemeMeasurement {
  const tokens = theme === 'dark' ? DARK : LIGHT;
  const card = token(tokens, 'bg-card');
  const overlay = token(tokens, 'bg-overlay');
  const accent = token(tokens, 'accent-violet');
  const header = (mesh: Rgb) => over(mesh, headerTintAlpha, card);
  return {
    header,
    hover: mesh => over(accent, reviewHoverAlpha, header(mesh)),
    chip: mesh => over(accent, chipWashAlpha, header(mesh)),
    primary: over(accent, primaryFillAlpha, overlay),
    overlay,
  };
}

/** The worst contrast ratio a foreground reaches over the measured Mesh colours. */
function worstCase(
  foreground: Rgb,
  background: (mesh: Rgb) => Rgb,
): { ratio: number; mesh: string } {
  let worst = { ratio: Number.POSITIVE_INFINITY, mesh: '' };
  for (const mesh of MESH_COLOURS) {
    const ratio = contrastRatio(foreground, background(hexToRgb(mesh)));
    if (ratio < worst.ratio) worst = { ratio, mesh };
  }
  return worst;
}

// ---------- Tests ------------------------------------------------------------

describe('the Review label on a composed node header (#2737)', () => {
  it.each(['dark', 'light'] as const)('paints the label with the text-safe violet step (%s)', theme => {
    // The graphic accent is icon-safe, not text-safe. A label is text.
    const tokens = theme === 'dark' ? DARK : LIGHT;
    expect(tokens.get(reviewLabelToken)).toBeDefined();
    expect(reviewLabelToken).not.toBe('accent-violet');
  });

  it.each(['dark', 'light'] as const)(
    'clears 4.5:1 at rest, hover and keyboard focus in the %s theme',
    theme => {
      const surface = measure(theme);
      const tokens = theme === 'dark' ? DARK : LIGHT;
      const foreground = token(tokens, reviewLabelToken);

      const rest = worstCase(foreground, surface.header);
      const hover = worstCase(foreground, surface.hover);
      // `:focus-visible` paints a cyan outline and changes no background, so
      // the focused label sits on the same background as the resting one.
      const focus = rest;

      expect(
        rest.ratio,
        `${theme}: the Review label is ${rgbToHex(foreground)} on the Mesh-tinted header (worst Mesh ${rest.mesh}) — ${rest.ratio.toFixed(2)}:1`,
      ).toBeGreaterThanOrEqual(AA_BODY_TEXT);
      expect(
        hover.ratio,
        `${theme}: the Review label on hover (worst Mesh ${hover.mesh}) — ${hover.ratio.toFixed(2)}:1`,
      ).toBeGreaterThanOrEqual(AA_BODY_TEXT);
      expect(
        focus.ratio,
        `${theme}: the Review label at keyboard focus — ${focus.ratio.toFixed(2)}:1`,
      ).toBeGreaterThanOrEqual(AA_BODY_TEXT);
    },
  );

  it.each(['dark', 'light'] as const)(
    'the baseline accent FAILS on the same composition, so this check is not vacuous (%s)',
    theme => {
      const surface = measure(theme);
      const tokens = theme === 'dark' ? DARK : LIGHT;
      const baseline = token(tokens, 'accent-violet');
      const rest = worstCase(baseline, surface.header);
      const hover = worstCase(baseline, surface.hover);
      expect(
        rest.ratio,
        `${theme}: the graphic accent on the Mesh-tinted header — ${rest.ratio.toFixed(2)}:1 (the text-safe step is what the label must use)`,
      ).toBeLessThan(AA_BODY_TEXT);
      expect(
        hover.ratio,
        `${theme}: the graphic accent on the hovered header — ${hover.ratio.toFixed(2)}:1`,
      ).toBeLessThan(AA_BODY_TEXT);
    },
  );
});

describe('the other violet labels on the same header (#2737)', () => {
  it.each(['dark', 'light'] as const)(
    'the automation chip clears 4.5:1 on its own wash in the %s theme',
    theme => {
      const surface = measure(theme);
      const tokens = theme === 'dark' ? DARK : LIGHT;
      const worst = worstCase(token(tokens, automationChipToken), surface.chip);
      expect(
        worst.ratio,
        `${theme}: the automation chip label — ${worst.ratio.toFixed(2)}:1`,
      ).toBeGreaterThanOrEqual(AA_BODY_TEXT);
    },
  );

  it.each(['dark', 'light'] as const)(
    'the resume glyph clears 4.5:1 on the header in the %s theme',
    theme => {
      const surface = measure(theme);
      const tokens = theme === 'dark' ? DARK : LIGHT;
      const worst = worstCase(token(tokens, resumeGlyphToken), surface.header);
      expect(
        worst.ratio,
        `${theme}: the resume glyph — ${worst.ratio.toFixed(2)}:1`,
      ).toBeGreaterThanOrEqual(AA_BODY_TEXT);
    },
  );
});

describe('the review dialog (#2737)', () => {
  it.each(['dark', 'light'] as const)(
    'the enabled primary action clears 4.5:1 in the %s theme',
    theme => {
      const surface = measure(theme);
      const tokens = theme === 'dark' ? DARK : LIGHT;
      const ratio = contrastRatio(token(tokens, reviewPrimaryToken), surface.primary);
      expect(
        ratio,
        `${theme}: "Start review" on ${rgbToHex(surface.primary)} — ${ratio.toFixed(2)}:1`,
      ).toBeGreaterThanOrEqual(AA_BODY_TEXT);
    },
  );

  it.each(['dark', 'light'] as const)(
    'the "View circuit run" link clears 4.5:1 in the %s theme',
    theme => {
      const surface = measure(theme);
      const tokens = theme === 'dark' ? DARK : LIGHT;
      const ratio = contrastRatio(token(tokens, reviewLinkToken), surface.overlay);
      expect(
        ratio,
        `${theme}: the circuit-run link on the dialog surface — ${ratio.toFixed(2)}:1`,
      ).toBeGreaterThanOrEqual(AA_BODY_TEXT);
    },
  );

  it.each(['dark', 'light'] as const)(
    'the baseline accent FAILS the dialog pairs in the %s theme',
    theme => {
      const surface = measure(theme);
      const tokens = theme === 'dark' ? DARK : LIGHT;
      const baseline = token(tokens, 'accent-violet');
      expect(
        contrastRatio(baseline, surface.primary),
        `${theme}: the graphic accent on the dialog's primary action — ${contrastRatio(baseline, surface.primary).toFixed(2)}:1`,
      ).toBeLessThan(AA_BODY_TEXT);
    },
  );
});

describe('the violet labels on the other surfaces (#2737)', () => {
  /**
   * Each entry names the surface the label is painted on and the component
   * that paints it. These are flat surfaces — no Mesh tint — so the worst
   * case is the surface itself.
   */
  const SITES: Array<{
    name: string;
    tokenName: string;
    surface: (tokens: Map<string, string>) => Rgb;
  }> = [
    {
      name: 'the Companion pill in the title bar',
      tokenName: companionPillToken,
      surface: tokens => token(tokens, 'bg-surface'),
    },
    {
      name: 'the Companion "Ask" action in the omnibar',
      tokenName: companionAskToken,
      surface: tokens => token(tokens, 'bg-overlay'),
    },
    {
      name: 'the Companion answer link in the omnibar',
      tokenName: companionLinkToken,
      surface: tokens => token(tokens, 'bg-overlay'),
    },
    {
      name: 'the sidebar resume glyph on hover',
      tokenName: sidebarResumeToken,
      surface: tokens => token(tokens, 'bg-card-hover'),
    },
    {
      name: 'the watched issue-label chip in the Probe',
      tokenName: issueLabelToken,
      surface: tokens => over(token(tokens, 'accent-violet'), 0.10, token(tokens, 'bg-base')),
    },
    {
      name: 'the "Edit Flow" chip in the Circuits Probe',
      tokenName: editFlowToken,
      surface: tokens => over(token(tokens, 'accent-violet'), 0.15, token(tokens, 'bg-base')),
    },
  ];

  it.each(SITES)('$name clears 4.5:1', ({ tokenName, surface }) => {
    for (const theme of ['dark', 'light'] as const) {
      const tokens = theme === 'dark' ? DARK : LIGHT;
      const ratio = contrastRatio(token(tokens, tokenName), surface(tokens));
      expect(
        ratio,
        `${theme}: ${tokenName} on its surface — ${ratio.toFixed(2)}:1`,
      ).toBeGreaterThanOrEqual(AA_BODY_TEXT);
    }
  });

  it.each(SITES)('$name uses the text-safe violet step', ({ tokenName }) => {
    expect(tokenName).not.toBe('accent-violet');
  });
});
