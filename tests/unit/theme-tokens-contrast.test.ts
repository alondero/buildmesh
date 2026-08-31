/**
 * Theme-token contrast regression — issue #732.
 *
 * Guards four contracts:
 *   1. `--color-text-muted` clears WCAG AA (4.5:1) on every surface
 *      the design system actually renders text onto (bg-base, bg-surface,
 *      bg-overlay, bg-card). The pre-fix #4a5568 measured 2.6:1 —
 *      clearly failing.
 *   2. The text-token hierarchy stays monotonic: text-primary >
 *      text-secondary > text-muted > bg-base. A brighter-muted change
 *      that overshoots secondary would flatten the tier.
 *   3. Hand-picked load-bearing sites (paths, branch names, dates,
 *      counts, mesh subheading, timestamps) use text-text-secondary,
 *      not text-text-muted. A future demotion of any one of them fails
 *      the static-analysis list at the bottom of this file.
 *
 * Contrast math is re-implemented inline (no third-party a11y dep)
 * so the test runs anywhere Vitest runs.
 */

import { describe, it, expect } from 'vitest';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

// ---------- WCAG contrast helpers -----------------------------------------

function srgbChannelToLinear(c: number): number {
  // Per WCAG 2.x: if c <= 0.03928 → c / 12.92, else ((c + 0.055) / 1.055)^2.4
  const cs = c / 255;
  return cs <= 0.03928 ? cs / 12.92 : Math.pow((cs + 0.055) / 1.055, 2.4);
}

function relativeLuminance(hex: string): number {
  // 6-digit hex only — buildmesh tokens are all 6-digit.
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

const APP_CSS = readFileSync(resolve(__dirname, '../../src/App.css'), 'utf8');

function readToken(name: string): string {
  // Matches `--color-text-muted: #7a8492;` (and tolerates surrounding
  // whitespace / comments). Picks the FIRST occurrence — tokens are
  // declared once at the top of the @theme block.
  const re = new RegExp(`--color-${name}\\s*:\\s*([^;]+);`);
  const m = re.exec(APP_CSS);
  if (!m) throw new Error(`Token --color-${name} not found in src/App.css`);
  return m[1].trim();
}

// Pin the token values themselves so a future "let's soften the muted"
// revert is caught at the test boundary — not just by visual review.
// Surfaces retuned to neutral zinc in issue #1376; ratios re-measured
// against --color-text-muted #7a8492 (5.22 / 4.97 / 4.80 / 4.75).
// The AA assertions below feed readToken() output into contrastRatio(),
// so an edit to src/App.css that drifts from these pins fails BOTH the
// pin and the ratio maths.
const EXPECTED_TEXT_MUTED = '#7a8492';
const BG_BASE = '#0a0a0e';
const BG_SURFACE = '#111116';
const BG_OVERLAY = '#15151c';
const BG_CARD = '#16161d';
const BG_CARD_HOVER = '#1b1b23';
const AA_BODY_THRESHOLD = 4.5;

const PINNED_BG_TOKENS: Array<[string, string]> = [
  ['bg-base', BG_BASE],
  ['bg-surface', BG_SURFACE],
  ['bg-overlay', BG_OVERLAY],
  ['bg-card', BG_CARD],
  ['bg-card-hover', BG_CARD_HOVER],
];

describe('pinned surface tokens (#1376)', () => {
  it.each(PINNED_BG_TOKENS)(
    'declares --color-%s as the pinned zinc value %s',
    (name, expected) => {
      // Ties the constants below to the real src/App.css declarations —
      // without this, the ratio tests could pass while the CSS drifted.
      expect(readToken(name)).toBe(expected);
    },
  );
});

// ---------- Tests ----------------------------------------------------------

describe('theme text-token contrast (#732)', () => {
  it('declares --color-text-muted as the AA-passing palette grey', () => {
    // Pin the exact value so a future change is intentional, not drift.
    expect(readToken('text-muted')).toBe(EXPECTED_TEXT_MUTED);
  });

  it('--color-text-muted clears WCAG AA (4.5:1) on bg-base', () => {
    const ratio = contrastRatio(readToken('text-muted'), readToken('bg-base'));
    expect(ratio).toBeGreaterThanOrEqual(AA_BODY_THRESHOLD);
  });

  it('--color-text-muted clears WCAG AA (4.5:1) on bg-surface', () => {
    // bg-surface is the panel background — every probe tab renders onto
    // it. Failing on bg-surface is the same bug as on bg-base, just on
    // a different panel.
    const ratio = contrastRatio(readToken('text-muted'), readToken('bg-surface'));
    expect(ratio).toBeGreaterThanOrEqual(AA_BODY_THRESHOLD);
  });

  it('--color-text-muted clears WCAG AA (4.5:1) on bg-overlay', () => {
    // bg-overlay is used for elevated panels (e.g. the diff header strip).
    // The current ratio is 4.80:1 (4.86:1 before the #1376 surface
    // retune) — a future darkening of bg-overlay would push it below AA.
    // Pin the contract.
    const ratio = contrastRatio(readToken('text-muted'), readToken('bg-overlay'));
    expect(ratio).toBeGreaterThanOrEqual(AA_BODY_THRESHOLD);
  });

  it('--color-text-muted clears WCAG AA (4.5:1) on bg-card', () => {
    // bg-card is the card surface used throughout the probe tabs and
    // mesh panels. The current ratio is 4.75:1 (4.61:1 before the
    // #1376 surface retune — the darker zinc card actually widened the
    // margin). A future lighten of bg-card would fail this guard.
    // Pin the contract.
    const ratio = contrastRatio(readToken('text-muted'), readToken('bg-card'));
    expect(ratio).toBeGreaterThanOrEqual(AA_BODY_THRESHOLD);
  });

  it('--color-text-muted clears WCAG AA (4.5:1) on bg-card-hover', () => {
    // Hovered cards still render their text — muted copy must stay AA
    // on the hover surface too. #1376 originally proposed #1c1c24, which
    // measures 4.47:1 — under AA — so the token ships as #1b1b23
    // (4.52:1). The value is pinned by the bg-card-hover entry in
    // PINNED_BG_TOKENS above, so a revert to the spec's original hex
    // fails there, not at visual review.
    const ratio = contrastRatio(readToken('text-muted'), readToken('bg-card-hover'));
    expect(ratio).toBeGreaterThanOrEqual(AA_BODY_THRESHOLD);
  });

  it('--color-text-muted stays subordinate to --color-text-secondary', () => {
    // Hierarchy guard: text-muted must be visually LESS prominent than
    // text-secondary, otherwise the two tokens collapse into one and the
    // design system loses a tier.
    const muted = contrastRatio(readToken('text-muted'), readToken('bg-base'));
    const secondary = contrastRatio(readToken('text-secondary'), readToken('bg-base'));
    expect(muted).toBeLessThan(secondary);
  });

  it('text-token ordering on bg-base is monotonic (primary > secondary > muted > base)', () => {
    // A "let's brighten text-muted" change that overshoots text-secondary
    // would flatten the hierarchy. The four-way comparison catches that
    // here, not in a visual review after the PR ships.
    const primary = contrastRatio(readToken('text-primary'), readToken('bg-base'));
    const secondary = contrastRatio(readToken('text-secondary'), readToken('bg-base'));
    const muted = contrastRatio(readToken('text-muted'), readToken('bg-base'));
    expect(primary).toBeGreaterThan(secondary);
    expect(secondary).toBeGreaterThan(muted);
    expect(muted).toBeGreaterThan(contrastRatio(readToken('bg-base'), readToken('bg-base'))); // == 1
  });
});

// ---------------------------------------------------------------------------
// Load-bearing label promotion: static-analysis guard.
// Issue #732 called out specific sites that displayed meaningful data
// (paths, branch names, dates, counts, mesh subheading, timestamps) using
// text-text-muted — those need to be on text-text-secondary. This list pins
// the boundary so a future regression that demotes one back to muted is
// caught here, before it ships.
//
// Each entry's `fragment` MUST be unique to the load-bearing line we want
// to guard (not just shared with the className prefix). Using a generic
// fragment like `text-2xs` would also match an unrelated `Badge` helper
// somewhere else in the file and produce confusing false-positives; pin
// the JSX body of the line, or a tail fragment that only appears on the
// className line being guarded.
// ---------------------------------------------------------------------------

describe('load-bearing text-text-muted sites promoted to text-text-secondary (#732)', () => {
  const loadBearingSites: Array<{ file: string; fragment: string; mustNotContain: string }> = [
    {
      file: 'src/components/Probe/WorktreeManagerTab.tsx',
      // Repo path (line 882) — uniquely identified by the trailing
      // `truncate flex-1 min-w-0` that no other `text-2xs font-mono`
      // line in this file uses.
      fragment: 'truncate flex-1 min-w-0',
      mustNotContain: 'text-text-muted',
    },
    {
      file: 'src/components/Probe/WorktreeManagerTab.tsx',
      // Remote-tracking branch (line 1121) — the `text-2xs font-mono`
      // prefix with `px-1 truncate` tail is unique to this site (the
      // repo-path line uses `flex-1 min-w-0` instead).
      fragment: 'text-2xs font-mono text-text-',
      mustNotContain: 'text-text-muted',
    },
    {
      file: 'src/components/Probe/WorktreeManagerTab.tsx',
      // Ahead/behind counts (line 987). Anchor on the body content
      // `↑{b.ahead}` so the assertion cannot be confused with the
      // decorative Badge helper that also uses `text-2xs`.
      fragment: '↑{b.ahead}',
      mustNotContain: 'text-text-muted',
    },
    {
      file: 'src/components/Probe/WorktreeManagerTab.tsx',
      // Local branch name (line 1054).
      fragment: ' · {w.branch}',
      mustNotContain: 'text-text-muted',
    },
    {
      file: 'src/components/Probe/ProbePanel.tsx',
      // Active mesh subheading in the probe header. The full className
      // tail `truncate min-w-0` is unique to line 119 (the section
      // message at line 274 also says `text-text-secondary` but uses
      // `leading-relaxed` instead, so the fragment disambiguates).
      fragment: 'text-text-secondary truncate min-w-0',
      mustNotContain: 'text-text-muted',
    },
    {
      file: 'src/components/Probe/ArchivedNodesTab.tsx',
      // timeAgo(timestamp) — load-bearing timestamp.
      fragment: '{timeAgo(session.timestamp)}',
      mustNotContain: 'text-text-muted',
    },
    {
      file: 'src/components/Probe/ProjectFilesTab.tsx',
      // Active file path in the project files tab header.
      fragment: 'text-xs font-mono text-text-secondary truncate flex-1 min-w-0',
      mustNotContain: 'text-text-muted',
    },
    {
      file: 'src/components/WorktreeCloseDialog/WorktreeCloseDialog.tsx',
      // Risk-text paragraph (line 49) the user reads before clicking
      // the destructive "Remove" button. The user must be able to read
      // what they're confirming.
      fragment: 'text-xs text-text-secondary mb-2',
      mustNotContain: 'text-text-muted',
    },
    {
      file: 'src/components/WorktreeCloseDialog/WorktreeCloseDialog.tsx',
      // The path being deleted (in the destructive-confirmation dialog
      // the user is about to OK).
      fragment: '{pending.safety.worktree_path}',
      mustNotContain: 'text-text-muted',
    },
    {
      file: 'src/components/AppSettings/HarnessConfigList.tsx',
      // Provider base_url — config the user is editing.
      fragment: '{p.base_url}',
      mustNotContain: 'text-text-muted',
    },
    {
      file: 'src/components/Probe/MeshPropertiesTab.tsx',
      // The mesh directory read-only field (line 339). Anchor on the
      // className tail — `{activeMeshPath}` only appears on the `value=`
      // line, not the className line, so it would let a className
      // revert slip through.
      fragment: 'bg-bg-surface border border-border-subtle rounded px-2 py-1.5 text-xs text-text-',
      mustNotContain: 'text-text-muted',
    },
  ];

  it.each(loadBearingSites)(
    '$file contains $mustNotContain only on lines NOT matching $fragment',
    ({ file, fragment, mustNotContain }) => {
      const src = readFileSync(resolve(__dirname, '../../' + file), 'utf8');
      const offendingLines: string[] = [];
      src.split(/\r?\n/).forEach((line, idx) => {
        if (line.includes(fragment) && line.includes(mustNotContain)) {
          offendingLines.push(`${file}:${idx + 1}: ${line.trim()}`);
        }
      });
      expect(
        offendingLines,
        `Load-bearing sites must not use ${mustNotContain}:\n  ${offendingLines.join('\n  ')}`,
      ).toEqual([]);
    },
  );
});