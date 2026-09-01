/**
 * `alignHunkRows` regression net (issue #1374). Pinned so a future
 * simplification doesn't regress GitHub's side-by-side contract on
 * interleaved / multi-block hunk shapes — these are the cases the
 * naive `removes.shift()` loop is easiest to break (e.g. "leading add
 * paired with a later remove" or "remove-run split by a context line
 * must not be visually merged with the context").
 *
 * The tests are pure: they don't render React. They build a `DiffHunk`
 * with `line_type` / `old_num` / `new_num` shaped like what the
 * backend produces, run `alignHunkRows`, and assert the row layout.
 */

import { describe, it, expect } from 'vitest';
import type { DiffHunk, DiffLine } from '../../src/lib/tauri';
import { alignHunkRows } from '../../src/components/Diff/diffFormat';

/** Tiny helper: a DiffLine with the type/nums the backend produces. */
function line(
  type: DiffLine['line_type'],
  content: string,
  oldNum: number | null,
  newNum: number | null,
): DiffLine {
  return { line_type: type, content, old_num: oldNum, new_num: newNum };
}

/** Build a minimal DiffHunk from a flat list of lines. */
function hunk(lines: DiffLine[]): DiffHunk {
  return {
    old_start: 1,
    old_lines: 0,
    new_start: 1,
    new_lines: 0,
    old_highlighted: '',
    new_highlighted: '',
    lines,
    lines_highlighted: [],
  };
}

describe('alignHunkRows (issue #1374)', () => {
  it('pairs a remove with its immediate add (basic case)', () => {
    const rows = alignHunkRows(
      hunk([
        line('remove', 'old', 1, null),
        line('add', 'new', null, 1),
      ]),
    );
    expect(rows).toHaveLength(1);
    expect(rows[0].old?.content).toBe('old');
    expect(rows[0].new?.content).toBe('new');
  });

  it('pairs a remove-run with an add-run 1:1 (two removes, two adds)', () => {
    const rows = alignHunkRows(
      hunk([
        line('remove', 'r1', 1, null),
        line('remove', 'r2', 2, null),
        line('add', 'a1', null, 1),
        line('add', 'a2', null, 2),
      ]),
    );
    expect(rows).toHaveLength(2);
    expect(rows[0].old?.content).toBe('r1');
    expect(rows[0].new?.content).toBe('a1');
    expect(rows[1].old?.content).toBe('r2');
    expect(rows[1].new?.content).toBe('a2');
  });

  it('flushes unpaired removes when a context line arrives', () => {
    // The classic "remove-run then context then remove then add"
    // shape. The first remove has no add partner by the time the
    // context line arrives — it must become its own remove-only row,
    // and the context must NOT be merged with it.
    const rows = alignHunkRows(
      hunk([
        line('remove', 'orphan', 1, null),
        line('context', 'unchanged', 2, 1),
        line('remove', 'r2', 3, null),
        line('add', 'a2', null, 2),
      ]),
    );
    expect(rows).toHaveLength(3);
    expect(rows[0].old?.content).toBe('orphan');
    expect(rows[0].new).toBeNull();
    expect(rows[1].old?.content).toBe('unchanged');
    expect(rows[1].new?.content).toBe('unchanged');
    expect(rows[2].old?.content).toBe('r2');
    expect(rows[2].new?.content).toBe('a2');
  });

  it('handles leading adds (no partner yet) before removes arrive', () => {
    // add, add, remove — the two adds must each be add-only rows
    // (left cell empty), then the remove becomes a remove-only row at
    // end-of-hunk. Naive `removes.shift()` would pair the FIRST add
    // with the remove, breaking the layout.
    const rows = alignHunkRows(
      hunk([
        line('add', 'a1', null, 1),
        line('add', 'a2', null, 2),
        line('remove', 'r1', 1, null),
      ]),
    );
    expect(rows).toHaveLength(3);
    expect(rows[0]).toEqual({ old: null, new: rows[0].new, oldHtml: undefined, newHtml: undefined });
    expect(rows[0].new?.content).toBe('a1');
    expect(rows[1].new?.content).toBe('a2');
    expect(rows[2].old?.content).toBe('r1');
    expect(rows[2].new).toBeNull();
  });

  it('handles a single trailing orphan remove', () => {
    const rows = alignHunkRows(
      hunk([line('remove', 'orphan', 1, null)]),
    );
    expect(rows).toHaveLength(1);
    expect(rows[0].old?.content).toBe('orphan');
    expect(rows[0].new).toBeNull();
  });

  it('pairs interleaved remove/add correctly across a context boundary', () => {
    // remove, add, context, remove, add — the first remove+add pair,
    // then a context row, then a SECOND remove+add pair. Each pair
    // must close on its own side of the context, not bleed across.
    // The algorithm produces 3 rows (paired, context, paired) — no
    // 4th because both pairs close cleanly across the context.
    const rows = alignHunkRows(
      hunk([
        line('remove', 'r1', 1, null),
        line('add', 'a1', null, 1),
        line('context', 'mid', 2, 2),
        line('remove', 'r2', 3, null),
        line('add', 'a2', null, 3),
      ]),
    );
    expect(rows).toHaveLength(3);
    expect(rows[0].old?.content).toBe('r1');
    expect(rows[0].new?.content).toBe('a1');
    expect(rows[1].old?.content).toBe('mid');
    expect(rows[1].new?.content).toBe('mid');
    expect(rows[2].old?.content).toBe('r2');
    expect(rows[2].new?.content).toBe('a2');
  });
});
