/** Status normalization and raw-patch parsing used by the diff surfaces. */

import { describe, it, expect } from 'vitest';
import {
  fileDiffStatusMeta,
  type FileDiffStatusMeta,
} from '../../src/lib/status';
import { parsePatchIntoHunks } from '../../src/components/Diff/diffFormat';

// ---------------------------------------------------------------------------
// Status meta — one canonical source, all consumers agree.
// ---------------------------------------------------------------------------

describe('fileDiffStatusMeta (canonical, lib/status.ts)', () => {
  it('returns a known meta for each FileDiffStatus', () => {
    const expected: Array<[string, FileDiffStatusMeta]> = [
      ['added', { letter: 'A', label: 'Added', color: 'text-accent-green', hex: '#22c55e', hexBg: 'rgba(34, 197, 94, 0.15)' }],
      ['modified', { letter: 'M', label: 'Modified', color: 'text-accent-amber', hex: '#f59e0b', hexBg: 'rgba(245, 158, 11, 0.15)' }],
      ['deleted', { letter: 'D', label: 'Deleted', color: 'text-accent-red', hex: '#ef4444', hexBg: 'rgba(239, 68, 68, 0.15)' }],
      ['renamed', { letter: 'R', label: 'Renamed', color: 'text-accent-violet', hex: '#8b5cf6', hexBg: 'rgba(139, 92, 246, 0.15)' }],
      ['untracked', { letter: '?', label: 'Untracked', color: 'text-text-muted', hex: '#7a8492', hexBg: '#1b1b23' }],
    ];
    for (const [status, meta] of expected) {
      expect(fileDiffStatusMeta(status)).toEqual(meta);
    }
  });

  it('normalizes porcelain and shorthand status codes', () => {
    expect(fileDiffStatusMeta('A')).toEqual(fileDiffStatusMeta('added'));
    expect(fileDiffStatusMeta('a')).toEqual(fileDiffStatusMeta('added'));
    expect(fileDiffStatusMeta('M')).toEqual(fileDiffStatusMeta('modified'));
    expect(fileDiffStatusMeta('m')).toEqual(fileDiffStatusMeta('modified'));
    expect(fileDiffStatusMeta('D')).toEqual(fileDiffStatusMeta('deleted'));
    expect(fileDiffStatusMeta('d')).toEqual(fileDiffStatusMeta('deleted'));
    expect(fileDiffStatusMeta('R')).toEqual(fileDiffStatusMeta('renamed'));
    expect(fileDiffStatusMeta('r')).toEqual(fileDiffStatusMeta('renamed'));
    expect(fileDiffStatusMeta('?')).toEqual(fileDiffStatusMeta('untracked'));
    expect(fileDiffStatusMeta('U')).toEqual(fileDiffStatusMeta('untracked'));
    expect(fileDiffStatusMeta('u')).toEqual(fileDiffStatusMeta('untracked'));
  });

  it('falls back to "modified" for unknown statuses (open-set vocabulary)', () => {
    // The generated `FileDiff.status` is a wider `string`; the function
    // accepts anything and tints it amber rather than rendering blank.
    expect(fileDiffStatusMeta('copied')).toEqual(fileDiffStatusMeta('modified'));
    expect(fileDiffStatusMeta('')).toEqual(fileDiffStatusMeta('modified'));
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
