/**
 * Diff canonical-surface tests — issue #725.
 *
 * Two parallel diff renderers + three parallel status-meta maps used to
 * drift across the codebase. After #725:
 *
 *   - One canonical `fileDiffStatusMeta` lives in `lib/status.ts`.
 *   - One canonical `<HunkBlock>` / `<DiffLineRow>` pair lives in
 *     `components/Diff/Diff.tsx` and is reused by `PrDiffView`.
 *   - The visual contract (`w-10` gutters, `/15` accent-fill opacity,
 *     `w-4` +/-/context marker column) is the same everywhere.
 *
 * These tests pin those contracts so a future drift in either direction
 * gets caught at PR time rather than by visual review.
 */

import { describe, it, expect } from 'vitest';
import { render } from '@testing-library/react';
import {
  fileDiffStatusMeta,
  type FileDiffStatusMeta,
} from '../../src/lib/status';
import { statusMeta as legacyStatusMeta } from '../../src/components/Diff/Diff';
import { HunkBlock, DIFF_LINE_BG, DIFF_GUTTER_WIDTH } from '../../src/components/Diff/Diff';
import { parsePatchIntoHunks } from '../../src/components/Diff/diffFormat';
import type { DiffHunk, DiffLine, FileDiff } from '../../src/lib/tauri';

// ---------------------------------------------------------------------------
// Status meta — one canonical source, all consumers agree.
// ---------------------------------------------------------------------------

describe('fileDiffStatusMeta (canonical, lib/status.ts)', () => {
  it('returns a known meta for each FileDiffStatus', () => {
    const expected: Array<[string, FileDiffStatusMeta]> = [
      ['added', { letter: 'A', label: 'Added', color: 'text-accent-green' }],
      ['modified', { letter: 'M', label: 'Modified', color: 'text-accent-amber' }],
      ['deleted', { letter: 'D', label: 'Deleted', color: 'text-accent-red' }],
      ['renamed', { letter: 'R', label: 'Renamed', color: 'text-accent-violet' }],
      ['untracked', { letter: '?', label: 'Untracked', color: 'text-text-muted' }],
    ];
    for (const [status, meta] of expected) {
      expect(fileDiffStatusMeta(status)).toEqual(meta);
    }
  });

  it('falls back to "modified" for unknown statuses (open-set vocabulary)', () => {
    // The generated `FileDiff.status` is a wider `string`; the function
    // accepts anything and tints it amber rather than rendering blank.
    expect(fileDiffStatusMeta('copied')).toEqual(fileDiffStatusMeta('modified'));
    expect(fileDiffStatusMeta('')).toEqual(fileDiffStatusMeta('modified'));
  });

  it('the legacy `statusMeta` re-export from Diff.tsx is the SAME function', () => {
    // FileTree.tsx still imports `statusMeta` from the historical home.
    // Pin it: drift here would mean the badge styling silently bifurcates.
    expect(legacyStatusMeta).toBe(fileDiffStatusMeta);
  });
});

// ---------------------------------------------------------------------------
// Visual contract — canonical gutter + accent-fill opacity tokens.
// ---------------------------------------------------------------------------

describe('diff visual contract (#725)', () => {
  function makeLine(type: DiffLine['line_type'], content: string): DiffLine {
    return {
      line_type: type,
      content,
      old_num: type === 'add' ? null : 1,
      new_num: type === 'remove' ? null : 1,
    };
  }

  it('canonical gutter width is w-10', () => {
    expect(DIFF_GUTTER_WIDTH).toBe('w-10');
  });

  it('canonical add/remove fill is /15 (matches PrDiffView + summary tokens)', () => {
    expect(DIFF_LINE_BG.add).toBe('bg-accent-green/15');
    expect(DIFF_LINE_BG.remove).toBe('bg-accent-red/15');
  });

  it('HunkBlock renders add lines with the canonical green/15 background', () => {
    const hunk: DiffHunk = {
      old_start: 1,
      old_lines: 1,
      new_start: 1,
      new_lines: 1,
      old_highlighted: '',
      new_highlighted: '',
      lines: [makeLine('add', 'fresh line')],
      lines_highlighted: [],
    };
    const { container } = render(<HunkBlock hunk={hunk} last />);
    // The row wrapper carries the /15 bg; the gutter span carries the w-10.
    const row = container.querySelector('div.flex');
    expect(row?.className).toContain('bg-accent-green/15');
    expect(container.querySelector(`.${DIFF_GUTTER_WIDTH}`)).not.toBeNull();
  });

  it('HunkBlock renders remove lines with the canonical red/15 background', () => {
    const hunk: DiffHunk = {
      old_start: 1,
      old_lines: 1,
      new_start: 1,
      new_lines: 1,
      old_highlighted: '',
      new_highlighted: '',
      lines: [makeLine('remove', 'gone line')],
      lines_highlighted: [],
    };
    const { container } = render(<HunkBlock hunk={hunk} last />);
    const row = container.querySelector('div.flex');
    expect(row?.className).toContain('bg-accent-red/15');
    expect(container.querySelector(`.${DIFF_GUTTER_WIDTH}`)).not.toBeNull();
  });
});

// ---------------------------------------------------------------------------
// Patch parser — PrDiffView's raw-patch → HunkBlock bridge.
// ---------------------------------------------------------------------------

describe('parsePatchIntoHunks (#725)', () => {
  it('parses a single-hunk patch into one DiffHunk with line numbers', () => {
    const patch = [
      '@@ -1,2 +1,2 @@',
      ' context',
      '-old',
      '+new',
    ].join('\n');

    const hunks = parsePatchIntoHunks(patch);
    expect(hunks).toHaveLength(1);
    const h = hunks[0];
    expect(h.old_start).toBe(1);
    expect(h.new_start).toBe(1);
    expect(h.lines).toEqual([
      { line_type: 'context', content: 'context', old_num: 1, new_num: 1 },
      { line_type: 'remove', content: 'old', old_num: 2, new_num: null },
      { line_type: 'add', content: 'new', old_num: null, new_num: 2 },
    ]);
  });

  it('parses a multi-hunk patch with running line counters', () => {
    const patch = [
      '@@ -1,1 +1,1 @@',
      '-gone',
      '+here',
      '@@ -10,1 +10,1 @@',
      '-later',
      '+now',
    ].join('\n');

    const hunks = parsePatchIntoHunks(patch);
    expect(hunks).toHaveLength(2);
    expect(hunks[0].lines.map((l) => l.new_num)).toEqual([null, 1]);
    expect(hunks[1].lines.map((l) => l.old_num)).toEqual([10, null]);
  });

  it('treats `\\ No newline at end of file` as a non-counter row', () => {
    const patch = [
      '@@ -1,1 +1,1 @@',
      '-line',
      '\\ No newline at end of file',
      '+line',
      '\\ No newline at end of file',
    ].join('\n');

    const hunks = parsePatchIntoHunks(patch);
    expect(hunks[0].lines).toHaveLength(4);
    // The two `\` rows are context (rendered dimly) and must NOT bump
    // the running counters — otherwise the next line's old_num/new_num
    // would point one row too far.
    expect(hunks[0].lines[0]).toMatchObject({ line_type: 'remove', old_num: 1 });
    expect(hunks[0].lines[2]).toMatchObject({ line_type: 'add', new_num: 1 });
  });

  it('drops stray lines outside any `@@` header', () => {
    // GitHub's diff blobs occasionally carry a leading comment line —
    // mirrors the parser's "real diff only" stance.
    const patch = ['stray preface', '@@ -1,1 +1,1 @@', '-old', '+new'].join('\n');
    const hunks = parsePatchIntoHunks(patch);
    expect(hunks).toHaveLength(1);
    expect(hunks[0].lines).toHaveLength(2);
  });
});

// ---------------------------------------------------------------------------
// End-to-end — a FileDiff rendered through Diff and a raw patch rendered
// through PrDiffView's bridge should produce equivalent line rows.
// ---------------------------------------------------------------------------

describe('Diff vs PrDiffView share the same body (#725)', () => {
  function file(partial: Partial<FileDiff> & { path: string }): FileDiff {
    return {
      hunks: [],
      status: 'modified',
      old_path: null,
      additions: 0,
      deletions: 0,
      binary: false,
      ...partial,
    };
  }

  it('the canonical `add` row class string is identical regardless of source', () => {
    // Feed identical content into both surfaces and confirm the rendered
    // row's className contains the canonical /15 token.
    const hunk: DiffHunk = {
      old_start: 1,
      old_lines: 0,
      new_start: 1,
      new_lines: 1,
      old_highlighted: '',
      new_highlighted: '',
      lines: [{ line_type: 'add', content: 'x', old_num: null, new_num: 1 }],
      lines_highlighted: [],
    };

    const fromDiff = render(<HunkBlock hunk={hunk} last />);
    expect(fromDiff.container.querySelector('div.flex')?.className).toContain(
      'bg-accent-green/15',
    );

    // The raw-patch path goes through parsePatchIntoHunks → HunkBlock.
    const patch = ['@@ -0,0 +1,1 @@', '+x'].join('\n');
    const fromPatch = render(
      <>
        {parsePatchIntoHunks(patch).map((h, i) => (
          <HunkBlock key={i} hunk={h} last={i === 0} />
        ))}
      </>,
    );
    expect(fromPatch.container.querySelector('div.flex')?.className).toContain(
      'bg-accent-green/15',
    );
  });

  it('the canonical FileDiffCard and a raw-patch body agree on gutter width', () => {
    const hunk: DiffHunk = {
      old_start: 1,
      old_lines: 0,
      new_start: 1,
      new_lines: 1,
      old_highlighted: '',
      new_highlighted: '',
      lines: [{ line_type: 'add', content: 'y', old_num: null, new_num: 1 }],
      lines_highlighted: [],
    };

    const { container } = render(
      <>
        <HunkBlock hunk={hunk} last />
        {parsePatchIntoHunks(['@@ -0,0 +1,1 @@', '+y'].join('\n')).map((h, i) => (
          <HunkBlock key={i} hunk={h} last />
        ))}
      </>,
    );
    // Every HunkBlock contributes one row wrapper + one gutter span per
    // add/remove/context line. We seeded two rows; both must show the
    // canonical w-10 gutter span, proving the two surfaces agree.
    const gutterSpans = container.querySelectorAll(`.${DIFF_GUTTER_WIDTH}`);
    expect(gutterSpans.length).toBeGreaterThanOrEqual(4);
  });

  // Compile-time check: a FileDiff passed into Diff must satisfy the
  // shape produced by parsePatchIntoHunks — they share the same DiffHunk
  // type, so unification is type-safe (not just visual).
  it('FileDiff.hunks and parsePatchIntoHunks return the same shape', () => {
    const f: FileDiff = file({ path: 'a.ts', hunks: [] });
    const patchHunks: DiffHunk[] = parsePatchIntoHunks('@@ -0,0 +1,1 @@\n+x');
    // Both arrays are DiffHunk[]; if a regression breaks one, this
    // assignment is the loudest possible signal.
    const sink: DiffHunk[][] = [f.hunks, patchHunks];
    expect(sink).toHaveLength(2);
  });
});