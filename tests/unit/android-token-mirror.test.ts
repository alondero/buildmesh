/**
 * The Android client's token mirror, recomputed — issue #2612.
 *
 * The native client used to hand-copy three things from the desktop and the
 * shared vocabulary: the Material colour roles (`MeshColors`), the type scale
 * (`BuildmeshTypography`), and the status vocabulary (`MeshColors.status`,
 * `workStatusShape`, `nodeLabel`). No test recomputed them, so the mirrors
 * drifted — the type scale had already left DESIGN.md's mobile scale — and the
 * status mirror failed soft: a status this build did not know rendered as a
 * muted grey dot with a generic label instead of admitting the gap.
 *
 * This file recomputes the mirror from the sources of truth:
 *   - the colour roles against the dark tokens in `src/App.css`
 *   - the status alphas against `src/mobile/styles.css`
 *   - the type scale against DESIGN.md's mobile scale
 *   - the status vocabulary against the generated table, which
 *     `scripts/generate-android-status-table.mjs` derives from `STATUS_CONFIG`
 *
 * The maps below name *which* source answers for *which* mirror entry. They
 * carry no numbers or colours of their own — the sizes come from DESIGN.md's
 * mobile scale and the colours from the stylesheets — so a retune in the
 * source fails here instead of drifting on the phone.
 */

import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { androidStatusTable, statusRows } from '../../scripts/generate-android-status-table.mjs';

const read = (path: string) => readFileSync(resolve(__dirname, '../..', path), 'utf8');

const THEME_KT = 'android/app/src/main/java/dev/buildmesh/remote/Theme.kt';
const GLYPH_KT = 'android/app/src/main/java/dev/buildmesh/remote/WorkStatusGlyph.kt';
const API_KT = 'android/app/src/main/java/dev/buildmesh/remote/BuildmeshApi.kt';
const STATUS_TABLE_KT = 'android/app/src/main/java/dev/buildmesh/remote/MeshStatusTable.kt';
const STATUS_TS = 'src/lib/status.ts';

// ---------- Kotlin source readers ------------------------------------------

/** The body of a Kotlin block that opens with `header`, up to its closing brace. */
function kotlinBlock(source: string, header: string): string {
  const start = source.indexOf(header);
  if (start === -1) throw new Error(`Not found in the Kotlin source: ${header}`);
  const open = source.indexOf('{', start);
  if (open === -1) throw new Error(`No block after: ${header}`);
  let depth = 0;
  for (let index = open; index < source.length; index += 1) {
    if (source[index] === '{') depth += 1;
    else if (source[index] === '}') {
      depth -= 1;
      if (depth === 0) return source.slice(open + 1, index);
    }
  }
  throw new Error(`Unbalanced block after: ${header}`);
}

/**
 * The body of a Kotlin function: the braced block, or the expression of an
 * expression-bodied `fun` (which has no braces to match).
 */
function kotlinBody(source: string, signature: string): string {
  const start = source.indexOf(signature);
  if (start === -1) throw new Error(`Not found in the Kotlin source: ${signature}`);
  const open = source.indexOf('{', start);
  const lineEnd = source.indexOf('\n', start);
  if (open === -1 || (lineEnd !== -1 && open > lineEnd)) {
    // An expression body: it runs to the end of the line.
    return source.slice(start, lineEnd === -1 ? source.length : lineEnd);
  }
  let depth = 0;
  for (let index = open; index < source.length; index += 1) {
    if (source[index] === '{') depth += 1;
    else if (source[index] === '}') {
      depth -= 1;
      if (depth === 0) return source.slice(open, index + 1);
    }
  }
  throw new Error(`Unbalanced body after: ${signature}`);
}

// ---------- Colour maths ----------------------------------------------------

interface Rgba {
  r: number;
  g: number;
  b: number;
  a: number;
}

/** A CSS colour token — `#rrggbb` or `rgba(r, g, b, a)`. */
function cssColour(value: string): Rgba {
  const hex = /^#([0-9a-f]{6})$/i.exec(value);
  if (hex) {
    return {
      r: parseInt(hex[1].slice(0, 2), 16),
      g: parseInt(hex[1].slice(2, 4), 16),
      b: parseInt(hex[1].slice(4, 6), 16),
      a: 1,
    };
  }
  const rgba = /^rgba?\(\s*([\d.]+)\s*,\s*([\d.]+)\s*,\s*([\d.]+)\s*(?:,\s*([\d.]+)\s*)?\)$/i.exec(value);
  if (rgba) {
    return {
      r: Number(rgba[1]),
      g: Number(rgba[2]),
      b: Number(rgba[3]),
      a: rgba[4] === undefined ? 1 : Number(rgba[4]),
    };
  }
  throw new Error(`Unsupported colour: ${value}`);
}

/** The colour a `MeshColors` entry paints, resolving an alpha copy of another entry. */
function meshColour(source: string, name: string): Rgba {
  const block = kotlinBlock(source, 'object MeshColors {');
  const literal = new RegExp(`^\\s*val ${name} = Color\\(0x([0-9a-f]{8})\\)$`, 'im').exec(block);
  if (literal) {
    const argb = literal[1];
    return {
      r: parseInt(argb.slice(2, 4), 16),
      g: parseInt(argb.slice(4, 6), 16),
      b: parseInt(argb.slice(6, 8), 16),
      a: parseInt(argb.slice(0, 2), 16) / 255,
    };
  }
  const white = new RegExp(`^\\s*val ${name} = Color\\.White\\.copy\\(alpha = ([\\d.]+)f?\\)$`, 'im').exec(block);
  if (white) return { r: 255, g: 255, b: 255, a: Number(white[1]) };
  const derived = new RegExp(`^\\s*val ${name} = (\\w+)\\.copy\\(alpha = ([\\d.]+)f?\\)$`, 'im').exec(block);
  if (derived) {
    const base = meshColour(source, derived[1]);
    return { ...base, a: Number(derived[2]) };
  }
  throw new Error(`MeshColors has no readable entry named ${name}`);
}

/** Two colours are the same when their channels match to the third decimal. */
function sameColour(left: Rgba, right: Rgba): boolean {
  return (
    left.r === right.r &&
    left.g === right.g &&
    left.b === right.b &&
    Math.abs(left.a - right.a) < 0.001
  );
}

function describeColour(colour: Rgba): string {
  return `rgb(${colour.r}, ${colour.g}, ${colour.b}) @ ${colour.a}`;
}

// ---------- CSS source readers ---------------------------------------------

/** A `--color-<name>` token from the dark theme in `src/App.css`. */
function appCssToken(source: string, name: string): string {
  const match = new RegExp(`--color-${name}\\s*:\\s*([^;]+);`).exec(source);
  if (!match) throw new Error(`Token --color-${name} is not declared in src/App.css`);
  return match[1].trim();
}

/** A `--<name>` custom property from the mobile stylesheet. */
function mobileToken(source: string, name: string): string {
  const match = new RegExp(`--${name}\\s*:\\s*([^;]+);`).exec(source);
  if (!match) throw new Error(`Token --${name} is not declared in src/mobile/styles.css`);
  return match[1].trim();
}

/** A colour property inside one CSS rule. */
function mobileRuleColour(source: string, rule: string, property: string): string {
  const block = new RegExp(`${rule.replace('.', '\\.')}\\s*\\{([^}]*)\\}`).exec(source);
  if (!block) throw new Error(`src/mobile/styles.css has no ${rule} rule`);
  const value = new RegExp(`${property}\\s*:\\s*([^;]+);`).exec(block[1]);
  if (!value) throw new Error(`${rule} sets no ${property}`);
  return value[1].trim();
}

// ---------- DESIGN.md's mobile scale ---------------------------------------

interface MobileScale {
  body: number;
  cardTitle: number;
  textField: number;
  metadata: number[];
  microLabel: number;
}

/**
 * The mobile type scale DESIGN.md states.
 *
 * The prose is the contract, so it is read from the prose: a reworded scale
 * fails here until the reader catches up, which is the point — the scale is
 * what the phone mirrors.
 */
function mobileScale(source: string): MobileScale {
  const paragraph = /\*\*Mobile scale\.\*\*\s*([\s\S]*?)\n\s*\n/.exec(source);
  if (!paragraph) throw new Error('DESIGN.md no longer states a mobile type scale');
  const text = paragraph[1].replace(/\s+/g, ' ');
  const step = (pattern: RegExp, what: string): number => {
    const match = pattern.exec(text);
    if (!match) throw new Error(`DESIGN.md's mobile scale no longer states the ${what} step`);
    return Number(match[1]);
  };
  const metadata = /(\d+)[–-](\d+)px metadata/.exec(text);
  if (!metadata) throw new Error("DESIGN.md's mobile scale no longer states the metadata range");
  return {
    body: step(/(\d+)px body/, 'body'),
    cardTitle: step(/(\d+)px card titles/, 'card title'),
    textField: step(/(\d+)px on text fields/, 'text field'),
    metadata: [Number(metadata[1]), Number(metadata[2])],
    microLabel: step(/(\d+)px floor for micro-labels/, 'micro-label floor'),
  };
}

/** The `MeshTypeScale` numbers Theme.kt mirrors the mobile scale into. */
function meshTypeScale(source: string): Map<string, number> {
  const block = kotlinBlock(source, 'object MeshTypeScale {');
  const scale = new Map<string, number>();
  for (const match of block.matchAll(/val\s+(\w+)\s*=\s*(\d+)/g)) scale.set(match[1], Number(match[2]));
  return scale;
}

/** The Material typography roles and the size each one asks for. */
function typographyRoles(source: string): Map<string, string> {
  const block = /private val BuildmeshTypography = Typography\(([\s\S]*?)\n\)/.exec(source);
  if (!block) throw new Error('Theme.kt has no BuildmeshTypography');
  const roles = new Map<string, string>();
  for (const match of block[1].matchAll(/(\w+)\s*=\s*textStyle\(([^,]+),/g)) {
    roles.set(match[1], match[2].trim());
  }
  return roles;
}

// ---------- The mirrors -----------------------------------------------------

/**
 * Which Material role answers for which step of the mobile scale.
 *
 * `bodyLarge` is Material's default text-field text, so it takes the text
 * field step; `titleMedium` and `titleSmall` name cards and card rows;
 * `bodySmall` and `labelMedium` are metadata; `labelSmall` is the micro-label
 * at the floor. The display and headline roles are page chrome above the
 * scale, and the test checks that relation rather than a number.
 */
const SCALE_ROLES: Record<string, string> = {
  titleLarge: 'textField',
  titleMedium: 'cardTitle',
  titleSmall: 'cardTitle',
  bodyLarge: 'textField',
  bodyMedium: 'body',
  bodySmall: 'metadata',
  labelLarge: 'cardTitle',
  labelMedium: 'metadataSmall',
  labelSmall: 'microLabel',
};

/** Which dark CSS token answers for which Material colour role. */
const MESH_COLOR_TOKENS: Record<string, string> = {
  background: 'bg-base',
  surface: 'bg-surface',
  overlay: 'bg-overlay',
  card: 'bg-card',
  cardHover: 'bg-card-hover',
  input: 'bg-input',
  selection: 'bg-selection',
  borderStrong: 'border-strong',
  borderDefault: 'border-default',
  borderSubtle: 'border-subtle',
  text: 'text-primary',
  secondary: 'text-secondary',
  muted: 'text-muted',
  accent: 'accent-cyan',
  violet: 'accent-violet',
  green: 'accent-green',
  amber: 'accent-amber',
  red: 'accent-red',
};

describe('the Android colour mirror (#2612)', () => {
  const appCss = read('src/App.css');
  const theme = read(THEME_KT);

  it.each(Object.entries(MESH_COLOR_TOKENS))(
    'MeshColors.%s mirrors the dark --color-%s token',
    (name, token) => {
      const expected = cssColour(appCssToken(appCss, token));
      const actual = meshColour(theme, name);
      expect(
        sameColour(actual, expected),
        `MeshColors.${name} is ${describeColour(actual)} but --color-${token} is ${describeColour(expected)}`,
      ).toBe(true);
    },
  );

  it('the needs-attention ring mirrors the mobile stylesheet --attention-ring', () => {
    const mobile = read('src/mobile/styles.css');
    const expected = cssColour(mobileToken(mobile, 'attention-ring'));
    const actual = meshColour(theme, 'attentionRing');
    expect(
      sameColour(actual, expected),
      `MeshColors.attentionRing is ${describeColour(actual)} but --attention-ring is ${describeColour(expected)}`,
    ).toBe(true);
  });

  it.each([
    ['approveBorder', '.deck-chip.approve'],
    ['rejectBorder', '.deck-chip.reject'],
  ])('MeshColors.%s mirrors the %s border', (name, rule) => {
    const mobile = read('src/mobile/styles.css');
    const expected = cssColour(mobileRuleColour(mobile, rule, 'border-color'));
    const actual = meshColour(theme, name);
    expect(
      sameColour(actual, expected),
      `MeshColors.${name} is ${describeColour(actual)} but ${rule} draws ${describeColour(expected)}`,
    ).toBe(true);
  });
});

describe('the Android type scale (#2612)', () => {
  const design = read('DESIGN.md');
  const theme = read(THEME_KT);

  it('MeshTypeScale mirrors the mobile scale DESIGN.md states', () => {
    const scale = mobileScale(design);
    const mirrored = meshTypeScale(theme);
    expect(mirrored.get('body')).toBe(scale.body);
    expect(mirrored.get('cardTitle')).toBe(scale.cardTitle);
    expect(mirrored.get('textField')).toBe(scale.textField);
    expect(mirrored.get('microLabel')).toBe(scale.microLabel);
    // The metadata step is a range, so the phone holds both of its ends.
    const bySize = (left: number, right: number) => left - right;
    expect(
      [mirrored.get('metadata'), mirrored.get('metadataSmall')].sort(bySize),
    ).toEqual([...scale.metadata].sort(bySize));
  });

  it.each(Object.entries(SCALE_ROLES))('the %s role reads the %s step', (role, step) => {
    const roles = typographyRoles(theme);
    expect(roles.get(role)).toBe(`MeshTypeScale.${step}`);
  });

  it('no role renders below the micro-label floor', () => {
    const scale = meshTypeScale(theme);
    const floor = scale.get('microLabel') ?? 0;
    for (const [role, size] of typographyRoles(theme)) {
      const resolved = size.startsWith('MeshTypeScale.')
        ? scale.get(size.slice('MeshTypeScale.'.length))
        : Number(size);
      expect(resolved, `${role} is ${size}`).toBeGreaterThanOrEqual(floor);
    }
  });

  it('the display and headline roles stay above the card-title step', () => {
    const scale = meshTypeScale(theme);
    const cardTitle = scale.get('cardTitle') ?? 0;
    for (const [role, size] of typographyRoles(theme)) {
      if (!/^(display|headline)/.test(role)) continue;
      const resolved = size.startsWith('MeshTypeScale.')
        ? scale.get(size.slice('MeshTypeScale.'.length))
        : Number(size);
      expect(resolved, `${role} is ${size}`).toBeGreaterThan(cardTitle);
    }
  });
});

describe('the Android status vocabulary (#2612)', () => {
  const statusSource = read(STATUS_TS);
  const table = read(STATUS_TABLE_KT);

  it('the committed table is what the generator produces from STATUS_CONFIG', () => {
    expect(table).toBe(androidStatusTable(statusSource));
  });

  it('carries the STATUS_CONFIG statuses with their label, colour, glyph and dot', () => {
    const rows = statusRows(statusSource);
    const block = /val entries: List<MeshStatus> = listOf\(([\s\S]*?)\n {4}\)/.exec(table);
    if (!block) throw new Error('The generated table has no entries list');
    const entries = block[1].match(/MeshStatus\([^)]*\)/g) ?? [];
    expect(entries).toHaveLength(rows.length);
    rows.forEach((row, index) => {
      expect(entries[index]).toBe(
        `MeshStatus("${row.status}", "${row.label}", 0xff${row.hex.slice(1).toLowerCase()}L, "${row.glyph}", "${row.dot}", ${row.pulsing})`,
      );
    });
  });

  it('names a status this build does not know an explicit gap', () => {
    expect(table).toContain('val Unknown = MeshStatus("unknown", "Unknown status", 0xffef4444L, "cross", "?", false)');
    expect(table).toContain('fun resolve(status: String): MeshStatus = byStatus[status] ?: Unknown');
  });

  it('escapes a label so Kotlin reads it literally', () => {
    // Kotlin reads a bare `$` as a template start and rejects a raw newline
    // inside a string, so the generator escapes both.
    const grown = statusSource.replace("    label: 'Ready',", "    label: 'Needs \"$5\" input',");
    expect(grown, 'the fixture must actually change a label').not.toBe(statusSource);
    expect(androidStatusTable(grown)).toContain('MeshStatus("ready", "Needs \\"\\$5\\" input", ');
  });

  it('resolves the vocabulary through the table instead of a hand-written branch', () => {
    const theme = read(THEME_KT);
    const glyph = read(GLYPH_KT);
    const api = read(API_KT);
    for (const [name, body] of [
      ['MeshColors.status', kotlinBody(theme, 'fun status(status: String)')],
      ['workStatusShape', kotlinBody(glyph, 'internal fun workStatusShape(status: String)')],
    ]) {
      expect(body, `${name} must not re-derive the vocabulary`).not.toContain('when');
      expect(body, `${name} must read the generated table`).toContain('MeshStatusTable.resolve(status)');
    }
    expect(api).toContain('MeshStatusTable.resolve(status).label');
    expect(api).not.toContain('status.replaceFirstChar');
  });
});

describe('the recompute fails on drift (#2612)', () => {
  it('a new status changes the table the phone must ship', () => {
    const source = read(STATUS_TS);
    const grown = source.replace(
      '  archived: {',
      `  brand_new: {
    color: 'text-accent-green',
    bgColor: 'bg-accent-green',
    dot: '✓',
    glyph: 'target',
    label: 'Brand new',
    hex: '#22c55e',
  },

  archived: {`,
    );
    expect(grown, 'the fixture must actually add a status').not.toBe(source);
    expect(androidStatusTable(grown)).not.toBe(androidStatusTable(source));
  });

  it('a retuned colour token changes the colour the mirror must hold', () => {
    const css = read('src/App.css');
    const retuned = css.replace('--color-accent-cyan: #00d4ff;', '--color-accent-cyan: #0891b2;');
    expect(retuned, 'the fixture must actually move a token').not.toBe(css);
    expect(appCssToken(retuned, 'accent-cyan')).not.toBe(appCssToken(css, 'accent-cyan'));
  });

  it('a retuned mobile scale changes the size the mirror must hold', () => {
    const design = read('DESIGN.md');
    const retuned = design.replace('13px body', '14px body');
    expect(retuned, 'the fixture must actually move a step').not.toBe(design);
    expect(mobileScale(retuned).body).not.toBe(mobileScale(design).body);
  });
});
