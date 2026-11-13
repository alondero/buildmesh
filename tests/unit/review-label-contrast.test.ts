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
 *
 * Issue #2745 — the two violet labels that live *outside* the node header are
 * measured here too, on the surfaces they actually sit on: the Agent History
 * worktree name (the Probe panel, and the row's hover wash) and the Circuits
 * join-category label (the node card's accent stripe, the inspector, the node
 * palette and the quick-connect menu). Each fill and each text token is read
 * with its own pattern out of the class list that identifies the element, so
 * the two readings are independent of the class order: swapping
 * `bg-accent-violet/20` with `text-accent-violet-strong` yields the same
 * measured ratios.
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

/** The first capture of a pattern in a component's own source. */
function capture(source: string, pattern: RegExp, where: string): string {
  const match = pattern.exec(source);
  if (!match) throw new Error(`Could not find ${where} in the component source`);
  return match[1];
}

/**
 * The text token a component paints with, read from the component's own
 * source — so a class change there moves this measurement with it.
 */
function foregroundToken(source: string, pattern: RegExp, where: string): string {
  return capture(source, pattern, where);
}

/**
 * A class list, located by what identifies the element rather than by the
 * order of its utilities.
 *
 * The violet fill's alpha and the violet text token are then read out of the
 * list with separate patterns, so the two readings are independent of each
 * other and of the class order (issue #2745).
 */
function classList(source: string, pattern: RegExp, where: string): string {
  return capture(source, pattern, where);
}

/** The violet fill's alpha, read from a class list on its own. */
function violetFillAlpha(list: string, where: string): number {
  const match = /bg-accent-violet\/(\d+)/.exec(list);
  if (!match) throw new Error(`Could not find the violet fill on ${where}`);
  return Number(match[1]) / 100;
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
/**
 * The dialog's primary-action class list ("Start review" / "Start Circuit").
 *
 * It is the class list that carries a *plain* violet fill. A variant-prefixed
 * utility — the trigger's `hover:bg-accent-violet/15` — is a state, not the
 * resting fill, so it is not one of these.
 */
const primaryActionClasses = (() => {
  const lists = [...REVIEW_BUTTON.matchAll(/className="([^"]*)"/g)].map(match => match[1]);
  const found = lists.filter(list => /(?:^|\s)bg-accent-violet/.test(list));
  if (found.length !== 1) {
    throw new Error(
      `Expected exactly one class list with a plain violet fill in AgentReviewButton.tsx, found ${found.length}`,
    );
  }
  return found[0];
})();

/** The dialog's primary action ("Start review" / "Start Circuit"). */
const reviewPrimaryToken = foregroundToken(
  primaryActionClasses,
  /text-(accent-violet(?:-strong)?)/,
  'the review dialog primary-action class',
);
/**
 * The automation chip on the node header.
 *
 * The chip's class list is read once and the fill and the token are read out
 * of it separately, so the two do not depend on each other's class name or on
 * the order the utilities are written in.
 */
const automationChipClasses = classList(
  GRID_HEADER,
  /automation: '([^']*bg-accent-violet[^']*)'/,
  'the automation chip class',
);
/** The automation chip on the node header. */
const automationChipToken = foregroundToken(
  automationChipClasses,
  /text-(accent-violet(?:-strong)?)/,
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
const ARCHIVED_NODES = read('src/components/Probe/ArchivedNodesTab.tsx');
const CIRCUIT_GRAPH_MODEL = read('src/components/Circuits/circuitGraphModel.ts');
const CIRCUIT_NODE_CARD = read('src/components/Circuits/CircuitNodeCard.tsx');

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
/**
 * The watched issue-label chip's class list — the branch of the `watched(label)`
 * conditional that carries the violet fill.
 */
const issueLabelChipClasses = classList(
  ISSUE_LABELS,
  /watched\(label\) \? '([^']*bg-accent-violet[^']*)'/,
  'the watched issue-label chip class',
);
/** The violet text of a watched issue label chip. */
const issueLabelToken = foregroundToken(
  issueLabelChipClasses,
  /text-(accent-violet(?:-strong)?)/,
  'the watched issue-label chip class',
);
/** The "Edit Flow" chip's class list. */
const editFlowChipClasses = classList(
  CIRCUITS_PROBE,
  /className="([^"]*bg-accent-violet[^"]*)"/,
  'the "Edit Flow" chip class',
);
/** The violet text of the "Edit Flow" chip in the Circuits Probe. */
const editFlowToken = foregroundToken(
  editFlowChipClasses,
  /text-(accent-violet(?:-strong)?)/,
  'the "Edit Flow" chip class',
);

/** The violet text of the worktree name in an Agent History row. */
const archiveWorktreeToken = foregroundToken(
  ARCHIVED_NODES,
  /text-2xs text-(accent-violet(?:-strong)?) font-mono/,
  'the Agent History worktree name class',
);
/**
 * The label colour of the Circuits "join" category — the one category whose
 * palette accent is also a text colour.
 */
const joinCategoryToken = (() => {
  const match = /case 'join':[\s\S]*?text: 'text-(accent-violet(?:-strong)?)'/.exec(CIRCUIT_GRAPH_MODEL);
  if (!match) throw new Error('Could not find the join category label class in circuitGraphModel.ts');
  return match[1];
})();

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

/**
 * The chip wash and the dialog primary fill: `bg-accent-violet/15`, `/20`.
 *
 * Each alpha is read from its own class list, on its own, so it does not
 * depend on the text token beside it nor on the order the two utilities are
 * written in (issue #2745).
 */
const chipWashAlpha = violetFillAlpha(automationChipClasses, 'the automation chip class');
const primaryFillAlpha = violetFillAlpha(primaryActionClasses, 'the review dialog primary-action class');
/** The watched issue-label chip's wash: `bg-accent-violet/10`. */
const issueLabelWashAlpha = violetFillAlpha(issueLabelChipClasses, 'the watched issue-label chip class');
/** The "Edit Flow" chip's wash: `bg-accent-violet/15`. */
const editFlowWashAlpha = violetFillAlpha(editFlowChipClasses, 'the "Edit Flow" chip class');
/** The node card's category stripe wash: `${accent.bg}/15`. */
const cardStripeAlpha = (() => {
  const match = /\$\{accent\.bg\}\/(\d+)/.exec(CIRCUIT_NODE_CARD);
  if (!match) throw new Error('Could not find the category stripe wash in CircuitNodeCard.tsx');
  return Number(match[1]) / 100;
})();

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
      surface: tokens => over(token(tokens, 'accent-violet'), issueLabelWashAlpha, token(tokens, 'bg-base')),
    },
    {
      name: 'the "Edit Flow" chip in the Circuits Probe',
      tokenName: editFlowToken,
      surface: tokens => over(token(tokens, 'accent-violet'), editFlowWashAlpha, token(tokens, 'bg-base')),
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

describe('the Agent History worktree name (#2745)', () => {
  /**
   * The worktree name is violet text in a Probe row. At rest the row sits on
   * the Probe panel, which is the app's `bg-bg-base`; hovering the row paints
   * `bg-bg-card`. The graphic accent measured 4.67:1 at rest and 4.25:1 on the
   * hover wash in the dark theme — under the 4.5:1 body-text floor.
   */
  it.each(['dark', 'light'] as const)(
    'clears 4.5:1 at rest and on the row hover in the %s theme',
    theme => {
      const tokens = theme === 'dark' ? DARK : LIGHT;
      const foreground = token(tokens, archiveWorktreeToken);
      const rest = contrastRatio(foreground, token(tokens, 'bg-base'));
      const hover = contrastRatio(foreground, token(tokens, 'bg-card'));
      expect(
        rest,
        `${theme}: the worktree name on the Probe panel — ${rest.toFixed(2)}:1`,
      ).toBeGreaterThanOrEqual(AA_BODY_TEXT);
      expect(
        hover,
        `${theme}: the worktree name on the hovered row — ${hover.toFixed(2)}:1`,
      ).toBeGreaterThanOrEqual(AA_BODY_TEXT);
    },
  );

  it('uses the text-safe violet step', () => {
    expect(archiveWorktreeToken).not.toBe('accent-violet');
  });

  it('the baseline accent FAILS on the hovered row, so this check is not vacuous', () => {
    const baseline = token(DARK, 'accent-violet');
    const hover = contrastRatio(baseline, token(DARK, 'bg-card'));
    expect(
      hover,
      `dark: the graphic accent on the hovered row — ${hover.toFixed(2)}:1`,
    ).toBeLessThan(AA_BODY_TEXT);
  });
});

describe('the Circuits join-category label (#2745)', () => {
  /**
   * The join category is the one whose palette accent is also a text colour,
   * and its label sits on four surfaces: the node card's category stripe, the
   * inspector, the node palette and the quick-connect menu (at rest and on
   * hover). The stripe is a *composed* surface — the category wash over the
   * card — so the label is measured against the composition, not the token.
   */
  const SURFACES: Array<{
    name: string;
    surface: (tokens: Map<string, string>) => Rgb;
  }> = [
    {
      name: 'the node card title strip',
      surface: tokens => over(token(tokens, 'accent-violet'), cardStripeAlpha, token(tokens, 'bg-card')),
    },
    {
      name: 'the inspector panel',
      surface: tokens => token(tokens, 'bg-surface'),
    },
    {
      name: 'the node palette',
      surface: tokens => token(tokens, 'bg-overlay'),
    },
    {
      name: 'the quick-connect menu',
      surface: tokens => token(tokens, 'bg-overlay'),
    },
    {
      name: 'the quick-connect menu on hover',
      surface: tokens => token(tokens, 'bg-card-hover'),
    },
  ];

  it.each(SURFACES)('$name clears 4.5:1', ({ surface }) => {
    for (const theme of ['dark', 'light'] as const) {
      const tokens = theme === 'dark' ? DARK : LIGHT;
      const ratio = contrastRatio(token(tokens, joinCategoryToken), surface(tokens));
      expect(
        ratio,
        `${theme}: the join-category label — ${ratio.toFixed(2)}:1`,
      ).toBeGreaterThanOrEqual(AA_BODY_TEXT);
    }
  });

  it('uses the text-safe violet step', () => {
    expect(joinCategoryToken).not.toBe('accent-violet');
  });

  it('the baseline accent FAILS on the card title strip, so this check is not vacuous', () => {
    for (const theme of ['dark', 'light'] as const) {
      const tokens = theme === 'dark' ? DARK : LIGHT;
      const baseline = token(tokens, 'accent-violet');
      const ratio = contrastRatio(baseline, over(token(tokens, 'accent-violet'), cardStripeAlpha, token(tokens, 'bg-card')));
      expect(
        ratio,
        `${theme}: the graphic accent on the card title strip — ${ratio.toFixed(2)}:1`,
      ).toBeLessThan(AA_BODY_TEXT);
    }
  });
});
