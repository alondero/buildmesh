/**
 * Filled action buttons — the destructive and accent recipes — issue #2775.
 *
 * The token-level contrast suites (#732 / #734) compare a text token with a
 * *neutral* surface. A filled action button does not sit on one: it paints
 * its own film and then draws a label on top of it, so the thing that has to
 * clear WCAG AA is the PAIR (fill, foreground) — and a translucent fill has
 * to be COMPOSITED against the surface beneath it before it is measured.
 *
 * That composition is exactly what the pre-fix recipe got wrong:
 *
 *   `text-white bg-status-error/80 hover:bg-status-error`
 *
 * white on the solid hover fill measured 3.76:1 in the dark theme (the
 * status-error red), and 3.77:1 in the light theme (the translucent normal
 * fill composites to a LIGHTER red over a white surface, so the *resting*
 * state was the broken one there). The cyan sibling failed harder: white on
 * accent-cyan measured 1.77:1 in the dark theme.
 *
 * This file measures the recipes as they are actually painted:
 *
 *   - the foreground token is read from the component's own class list, so a
 *     class change in the component moves this measurement with it;
 *   - the fill of each state (normal / hover / pressed / disabled) is read
 *     the same way and composited over the surface the button actually sits
 *     on — the dialog panel, the settings panel, the entry card, the discard
 *     banner's own wash;
 *   - the baseline is measured too and asserted to FAIL, so a check that
 *     passed for the wrong reason could not look like a regression test.
 */

import { describe, it, expect } from 'vitest';
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, relative, resolve } from 'node:path';

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
  const channel = (v: number) => Math.round(v).toFixed(0).padStart(2, '0');
  return `#${channel(r)}${channel(g)}${channel(b)}`;
}

// ---------- Sources of truth -----------------------------------------------

const REPO_ROOT = resolve(__dirname, '../..');

const read = (path: string) => readFileSync(resolve(REPO_ROOT, path), 'utf8');

/** The component sources, so the guard below also covers a component that is
 *  not in the inventory. */
function componentFiles(dir: string = join(REPO_ROOT, 'src', 'components')): string[] {
  return readdirSync(dir).flatMap((name) => {
    const path = join(dir, name);
    if (statSync(path).isDirectory()) return componentFiles(path);
    return name.endsWith('.tsx') ? [path] : [];
  });
}

/**
 * The single-, double- and backtick-quoted literals on one line. A class list
 * can be split over several lines, so the scan is over literals rather than
 * over lines.
 */
function classLiterals(source: string): string[] {
  const found: string[] = [];
  for (const match of source.matchAll(/'([^'\n]*)'|"([^"\n]*)"|`([^`\n]*)`/g)) {
    found.push(match[1] ?? match[2] ?? match[3] ?? '');
  }
  return found;
}

const APP_CSS = read('src/App.css');

/** The colour tokens of one theme: the dark `@theme` block, or the light override. */
function themeTokens(theme: 'dark' | 'light'): Map<string, string> {
  const scope =
    theme === 'dark'
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

// ---------- The recipes -----------------------------------------------------

/**
 * The fill tokens a filled action button may use. `status-error` is in the
 * list so the BASELINE recipe (the pre-fix white-on-error pair) can be
 * measured and shown to fail.
 */
const FILL_TOKENS = [
  'destructive',
  'destructive-strong',
  'accent-cyan',
  'accent-blue',
  'status-error',
  'bg-card-hover',
];

/** The foreground tokens a filled action button may paint its label with. */
const FOREGROUND_TOKENS = ['on-destructive', 'on-accent-cyan', 'text-inverse', 'text-muted'];

interface Fill {
  /** The `--color-*` token the fill comes from. */
  token: string;
  /** The alpha the fill is painted at (1 = solid). */
  alpha: number;
}

interface Recipe {
  /** The foreground token of the label. */
  foreground: string;
  /** The resting fill. */
  normal: Fill;
  /** The hover fill (falls back to the resting fill). */
  hover: Fill | null;
  /** The pressed fill (falls back to the hover fill). */
  pressed: Fill | null;
  /** The disabled fill, when the button has a dedicated one. */
  disabled: Fill | null;
  /** The foreground of the disabled state, when the button overrides it. */
  disabledForeground: string | null;
  /** True when the button fades the whole control instead of restyling it. */
  fadesWhenDisabled: boolean;
}

const FILL_PATTERN = new RegExp(`^(?:bg-)?(${FILL_TOKENS.join('|')})(?:/(\\d+))?$`);
const FOREGROUND_PATTERN = new RegExp(`^(?:text-)?(${FOREGROUND_TOKENS.join('|')})$`);

function parseFill(className: string): Fill | null {
  const match = FILL_PATTERN.exec(className);
  if (!match) return null;
  return { token: match[1], alpha: match[2] ? Number(match[2]) / 100 : 1 };
}

function parseRecipe(classList: string): Recipe | null {
  const classes = classList.split(/\s+/).filter(Boolean);
  let foreground: string | null = null;
  let normal: Fill | null = null;
  let hover: Fill | null = null;
  let pressed: Fill | null = null;
  let disabled: Fill | null = null;
  let disabledForeground: string | null = null;
  let fadesWhenDisabled = false;

  for (const cls of classes) {
    if (cls.startsWith('hover:')) {
      const fill = parseFill(cls.slice('hover:'.length));
      if (fill) hover = fill;
      continue;
    }
    if (cls.startsWith('active:')) {
      const fill = parseFill(cls.slice('active:'.length));
      if (fill) pressed = fill;
      continue;
    }
    if (cls.startsWith('disabled:')) {
      const rest = cls.slice('disabled:'.length);
      if (/^opacity-/.test(rest)) {
        // The whole-control fade is what made the disabled label unreadable:
        // a white label at half opacity over a half-faded red fill measures
        // 2.07:1 on the light theme's white panel — the label disappears.
        fadesWhenDisabled = true;
        continue;
      }
      const fill = parseFill(rest);
      if (fill) {
        disabled = fill;
        continue;
      }
      const fg = FOREGROUND_PATTERN.exec(rest);
      if (fg) disabledForeground = fg[1];
      continue;
    }
    const fill = parseFill(cls);
    if (fill) {
      normal = fill;
      continue;
    }
    const fg = FOREGROUND_PATTERN.exec(cls);
    if (fg) foreground = fg[1];
  }

  if (!foreground || !normal) return null;
  return { foreground, normal, hover, pressed, disabled, disabledForeground, fadesWhenDisabled };
}

/**
 * The class lists in `source` that paint one of the recipe foregrounds.
 * The class lists are read from the components rather than restated here,
 * so a component that changes its recipe moves this measurement with it.
 */
function recipeClassLists(source: string, foreground: string): string[] {
  const found: string[] = [];
  for (const match of source.matchAll(/'([^'\n]*)'|"([^"\n]*)"/g)) {
    const literal = match[1] ?? match[2];
    if (!literal.includes(`text-${foreground}`)) continue;
    if (!FILL_TOKENS.some((t) => literal.includes(`bg-${t}`))) continue;
    found.push(literal);
  }
  return found;
}

// ---------- The inventory ---------------------------------------------------

/**
 * The filled action buttons and the surface each one actually sits on.
 *
 * `surface` is the surface BEHIND the button, so a translucent fill is
 * composited against the right colour:
 *
 *   panel   the shared `<Modal>` panel — `bg-bg-overlay`
 *   card    a card inside that panel — `bg-bg-card`
 *   surface the flat panel surface — `bg-bg-surface`
 *   base    the app background — `bg-bg-base`
 *   banner  the discard banner's own wash — `bg-status-warning/10` over the
 *           shared modal panel
 *   editor-banner  the Circuit editor's discard banner — the same wash over
 *           the editor's `bg-bg-base` overlay
 */
type Surface = 'panel' | 'card' | 'surface' | 'base' | 'banner' | 'editor-banner';

interface Site {
  name: string;
  file: string;
  /** The foreground token the button's label is painted with. */
  foreground: 'on-destructive' | 'on-accent-cyan';
  surface: Surface;
}

const SITES: Site[] = [
  {
    name: 'the ConfirmDialog confirm button',
    file: 'src/components/ConfirmDialog/ConfirmDialog.tsx',
    foreground: 'on-destructive',
    surface: 'panel',
  },
  {
    name: 'the worktree close confirmation',
    file: 'src/components/WorktreeCloseDialog/WorktreeCloseDialog.tsx',
    foreground: 'on-destructive',
    surface: 'panel',
  },
  {
    name: 'the exit confirmation',
    file: 'src/components/ExitConfirmationModal/ExitConfirmationModal.tsx',
    foreground: 'on-destructive',
    surface: 'panel',
  },
  {
    name: 'the shared modal discard banner',
    file: 'src/components/shared/Modal.tsx',
    foreground: 'on-destructive',
    surface: 'banner',
  },
  {
    name: 'the Circuit editor discard banner',
    file: 'src/components/Circuits/CircuitFlowEditor.tsx',
    foreground: 'on-destructive',
    surface: 'editor-banner',
  },
  {
    name: 'the account remove confirmation',
    file: 'src/components/AppSettings/AccountCard.tsx',
    foreground: 'on-destructive',
    surface: 'panel',
  },
  {
    name: 'the data-recovery restore action',
    file: 'src/components/AppSettings/DataRecoverySection.tsx',
    foreground: 'on-destructive',
    surface: 'panel',
  },
  {
    name: 'the OpenCode sign-out confirmation',
    file: 'src/components/AppSettings/OpenCodeAccountViews.tsx',
    foreground: 'on-destructive',
    surface: 'panel',
  },
  {
    name: 'the remote-access revoke confirmation',
    file: 'src/components/AppSettings/RemoteAccessPane.tsx',
    foreground: 'on-destructive',
    surface: 'panel',
  },
  {
    name: 'the blocked-cleanup Retry button',
    file: 'src/components/BlockedCleanupDialog/BlockedCleanupDialog.tsx',
    foreground: 'on-accent-cyan',
    surface: 'card',
  },
  {
    name: 'the boot error panel reload button',
    file: 'src/components/BootErrorPanel/BootErrorPanel.tsx',
    foreground: 'on-accent-cyan',
    surface: 'surface',
  },
  {
    name: 'the error boundary reload button',
    file: 'src/components/ErrorBoundary/ErrorBoundary.tsx',
    foreground: 'on-accent-cyan',
    surface: 'surface',
  },
  {
    name: 'the prompt dialog action button',
    file: 'src/components/Prompts/PromptDialogs.tsx',
    foreground: 'on-accent-cyan',
    surface: 'panel',
  },
  {
    name: 'the empty-canvas spawn button',
    file: 'src/components/AgentNodeView/CanvasEmptyState.tsx',
    foreground: 'on-accent-cyan',
    surface: 'base',
  },
  {
    name: 'the node drag intent chip',
    file: 'src/components/AgentNodeView/nodeDrag.tsx',
    foreground: 'on-accent-cyan',
    surface: 'base',
  },
];

function surfaceColour(tokens: Map<string, string>, surface: Surface): Rgb {
  switch (surface) {
    case 'panel':
      return token(tokens, 'bg-overlay');
    case 'card':
      return token(tokens, 'bg-card');
    case 'surface':
      return token(tokens, 'bg-surface');
    case 'base':
      return token(tokens, 'bg-base');
    case 'banner':
      return over(token(tokens, 'status-warning'), 0.1, token(tokens, 'bg-overlay'));
    case 'editor-banner':
      return over(token(tokens, 'status-warning'), 0.1, token(tokens, 'bg-base'));
  }
}

/** The surfaces the filled action buttons in this app are painted on. */
const ALL_SURFACES: Surface[] = [
  'panel',
  'card',
  'surface',
  'base',
  'banner',
  'editor-banner',
];

const AA_BODY_TEXT = 4.5;

// ---------- Tests ------------------------------------------------------------

describe('the destructive and accent action recipes (#2775)', () => {
  it.each(['dark', 'light'] as const)(
    'the destructive recipe clears 4.5:1 on every surface in the %s theme',
    theme => {
      const tokens = theme === 'dark' ? DARK : LIGHT;
      const foreground = token(tokens, 'on-destructive');
      const normal = token(tokens, 'destructive');
      const hover = token(tokens, 'destructive');
      const pressed = token(tokens, 'destructive-strong');

      for (const surface of ALL_SURFACES) {
        const base = surfaceColour(tokens, surface);
        const states: Array<[string, Rgb]> = [
          ['normal', over(normal, 0.8, base)],
          ['hover', hover],
          ['pressed', pressed],
        ];
        for (const [state, background] of states) {
          const ratio = contrastRatio(foreground, background);
          expect(
            ratio,
            `${theme}: the destructive ${state} state on ${surface} — ${rgbToHex(background)} — ${ratio.toFixed(2)}:1`,
          ).toBeGreaterThanOrEqual(AA_BODY_TEXT);
        }
      }
    },
  );

  it.each(['dark', 'light'] as const)(
    'the accent recipe clears 4.5:1 on every surface in the %s theme',
    theme => {
      const tokens = theme === 'dark' ? DARK : LIGHT;
      const foreground = token(tokens, 'on-accent-cyan');
      const normal = token(tokens, 'accent-cyan');
      const hover = token(tokens, 'accent-cyan');
      const pressed = token(tokens, 'accent-blue');

      for (const surface of ALL_SURFACES) {
        const base = surfaceColour(tokens, surface);
        const states: Array<[string, Rgb]> = [
          ['normal', over(normal, 0.8, base)],
          ['hover', hover],
          ['pressed', pressed],
        ];
        for (const [state, background] of states) {
          const ratio = contrastRatio(foreground, background);
          expect(
            ratio,
            `${theme}: the accent ${state} state on ${surface} — ${rgbToHex(background)} — ${ratio.toFixed(2)}:1`,
          ).toBeGreaterThanOrEqual(AA_BODY_TEXT);
        }
      }
    },
  );

  it.each(['dark', 'light'] as const)(
    'the disabled label clears 4.5:1 in the %s theme',
    theme => {
      const tokens = theme === 'dark' ? DARK : LIGHT;
      const ratio = contrastRatio(token(tokens, 'text-muted'), token(tokens, 'bg-card-hover'));
      expect(
        ratio,
        `${theme}: the disabled label on the neutral disabled fill — ${ratio.toFixed(2)}:1`,
      ).toBeGreaterThanOrEqual(AA_BODY_TEXT);
    },
  );
});

describe('the filled action buttons in the app (#2775)', () => {
  for (const site of SITES) {
    it(`${site.name} clears 4.5:1 in normal, hover and pressed, in both themes`, () => {
      const classLists = recipeClassLists(read(site.file), site.foreground);
      expect(
        classLists.length,
        `No ${site.foreground} button found in ${site.file} — the site moved or was removed.`,
      ).toBeGreaterThan(0);

      for (const classList of classLists) {
        const recipe = parseRecipe(classList);
        expect(recipe, `Could not read the recipe out of "${classList}"`).not.toBeNull();
        if (!recipe) continue;

        for (const theme of ['dark', 'light'] as const) {
          const tokens = theme === 'dark' ? DARK : LIGHT;
          const foreground = token(tokens, recipe.foreground);
          const base = surfaceColour(tokens, site.surface);

          const states: Array<[string, Rgb]> = [
            ['normal', over(token(tokens, recipe.normal.token), recipe.normal.alpha, base)],
            [
              'hover',
              recipe.hover
                ? over(token(tokens, recipe.hover.token), recipe.hover.alpha, base)
                : over(token(tokens, recipe.normal.token), recipe.normal.alpha, base),
            ],
            [
              'pressed',
              recipe.pressed
                ? over(token(tokens, recipe.pressed.token), recipe.pressed.alpha, base)
                : recipe.hover
                  ? over(token(tokens, recipe.hover.token), recipe.hover.alpha, base)
                  : over(token(tokens, recipe.normal.token), recipe.normal.alpha, base),
            ],
          ];

          for (const [state, background] of states) {
            const ratio = contrastRatio(foreground, background);
            expect(
              ratio,
              `${theme}: ${site.name} — ${state} state on ${rgbToHex(background)} — ${ratio.toFixed(2)}:1 (classes: ${classList})`,
            ).toBeGreaterThanOrEqual(AA_BODY_TEXT);
          }
        }
      }
    });
  }

  it('a disabled filled action button keeps a readable label', () => {
    for (const site of SITES) {
      for (const classList of recipeClassLists(read(site.file), site.foreground)) {
        const recipe = parseRecipe(classList);
        if (!recipe) continue;

        // The whole-control fade is the failure mode: a white label at half
        // opacity over a half-faded red fill lands at 2.07:1 on the light
        // theme's white panel.
        expect(
          recipe.fadesWhenDisabled,
          `${site.file} fades the whole control when disabled — the label stops being readable: ${classList}`,
        ).toBe(false);

        if (!recipe.disabled) continue;
        expect(
          recipe.disabled.token,
          `${site.file} should restyle a disabled filled button rather than fade it: ${classList}`,
        ).toBe('bg-card-hover');
        expect(
          recipe.disabledForeground,
          `${site.file} should name the disabled label's foreground: ${classList}`,
        ).toBe('text-muted');

        for (const theme of ['dark', 'light'] as const) {
          const tokens = theme === 'dark' ? DARK : LIGHT;
          const ratio = contrastRatio(
            token(tokens, recipe.disabledForeground!),
            token(tokens, recipe.disabled.token),
          );
          expect(
            ratio,
            `${theme}: ${site.name} — the disabled label — ${ratio.toFixed(2)}:1`,
          ).toBeGreaterThanOrEqual(AA_BODY_TEXT);
        }
      }
    }
  });
});

describe('the action-fill tokens (#2775)', () => {
  // Pin the values so a retune is a deliberate change, not drift: the
  // measured ratios above are only true for these steps.
  it('pins the dark-theme steps', () => {
    expect(DARK.get('destructive')).toBe('#dc2626');
    expect(DARK.get('destructive-strong')).toBe('#b91c1b');
    expect(DARK.get('on-destructive')).toBe('#ffffff');
    expect(DARK.get('on-accent-cyan')).toBe('#0a0a0e');
  });

  it('pins the light-theme steps', () => {
    // The light theme needs the deeper destructive fill: a translucent red
    // over a white surface composites LIGHTER, so the same 80% fill that
    // passes in the dark theme would not pass here.
    expect(LIGHT.get('destructive')).toBe('#b91c1b');
    expect(LIGHT.get('destructive-strong')).toBe('#991b1b');
    expect(LIGHT.get('on-destructive')).toBe('#ffffff');
    // The brand cyan is light in BOTH themes, so its label is always dark.
    expect(LIGHT.get('on-accent-cyan')).toBe('#0a0a0e');
  });

  it('keeps the accent label dark in both themes', () => {
    // `text-inverse` is the wrong token for a cyan fill: it flips to white
    // in the light theme, which is how the cyan buttons lost their labels.
    expect(DARK.get('on-accent-cyan')).toBe(DARK.get('text-inverse'));
    expect(LIGHT.get('on-accent-cyan')).not.toBe(LIGHT.get('text-inverse'));
  });
});

describe('the baseline the fix replaced (#2775)', () => {
  it.each(['dark', 'light'] as const)(
    'the old white-on-error recipe FAILS on the same surfaces, so this check is not vacuous (%s)',
    theme => {
      const tokens = theme === 'dark' ? DARK : LIGHT;
      const white = hexToRgb('#ffffff');
      const error = token(tokens, 'status-error');
      const panel = surfaceColour(tokens, 'panel');

      // normal: the translucent fill, composited over the panel
      const normal = contrastRatio(white, over(error, 0.8, panel));
      // hover: the solid fill
      const hover = contrastRatio(white, error);

      expect(
        Math.min(normal, hover),
        `${theme}: the baseline recipe measured ${normal.toFixed(2)}:1 (normal) and ${hover.toFixed(2)}:1 (hover)`,
      ).toBeLessThan(AA_BODY_TEXT);
    },
  );

  it.each(['dark', 'light'] as const)(
    'the old white-on-cyan recipe FAILS in the %s theme',
    theme => {
      const tokens = theme === 'dark' ? DARK : LIGHT;
      const white = hexToRgb('#ffffff');
      const cyan = token(tokens, 'accent-cyan');
      const ratio = contrastRatio(white, cyan);
      expect(
        ratio,
        `${theme}: white on the solid accent fill — ${ratio.toFixed(2)}:1`,
      ).toBeLessThan(AA_BODY_TEXT);
    },
  );
});

describe('the white-on-error recipe does not come back (#2775)', () => {
  it('no component paints a white label on a filled error or accent button', () => {
    // The components under src/components are scanned, not only the sites in
    // the inventory above: a new destructive or accent button is where the
    // old pair would come back. The caption-button red (ADR-0035) is a fixed
    // system accent with its own tokens, and it is not one of these fill
    // tokens.
    const offenders: string[] = [];
    for (const file of componentFiles()) {
      for (const literal of classLiterals(readFileSync(file, 'utf8'))) {
        if (!literal.includes('text-white')) continue;
        if (!FILL_TOKENS.some((t) => literal.includes(`bg-${t}`))) continue;
        offenders.push(`${relative(REPO_ROOT, file)}: ${literal.trim()}`);
      }
    }
    expect(
      offenders,
      `White-on-fill labels are unreadable on the error and accent hues:\n  ${offenders.join('\n  ')}`,
    ).toEqual([]);
  });
});
