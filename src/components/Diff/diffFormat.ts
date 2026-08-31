/**
 * Shared diff formatting helpers — extracted from `Diff.tsx` and
 * `PrDiffView.tsx` to avoid two-place edits when a path-split rule
 * changes (e.g. Windows separators per issue #241, or a future rename).
 * `splitPath` was byte-identical in both files; this is its only home.
 *
 * Status meta: previously duplicated across three callers (#725), now
 * lives in `lib/status.ts` as `fileDiffStatusMeta`. Keep diff-format
 * helpers here so path rules and patch parsing evolve in one place.
 */

import type { DiffHunk, DiffLine } from '../../lib/tauri';

/** One aligned row of a side-by-side (split) diff — issue #1374.
 *
 *  A unified diff interleaves removes and adds; a split view must render
 *  them as two columns of the SAME row. `alignHunkRows` pairs them up:
 *  a remove+add run becomes one row with both cells populated, a remove
 *  with no partner leaves the right cell empty, an add with no partner
 *  leaves the left cell empty, and a context line populates both cells.
 *
 *  `old_num` / `new_num` are carried through from the unified lines, so
 *  each pane's gutter shows its own line numbering (left = old file,
 *  right = new file) with no desynchronization. */
export interface SplitRow {
  /** Left (old file) cell — null when the row is an add-only row. */
  old: DiffLine | null;
  /** Right (new file) cell — null when the row is a remove-only row. */
  new: DiffLine | null;
  /** Highlighted HTML for the left cell, aligned with `old` (from the
   *  hunk's `lines_highlighted` — preserved syntect tokens, issue #1374). */
  oldHtml?: string;
  /** Highlighted HTML for the right cell, aligned with `new`. */
  newHtml?: string;
}

/** Pair a hunk's unified lines into side-by-side rows.
 *
 *  Greedy pairing, GitHub-style: walk the hunk's lines in order; when a
 *  remove is followed (immediately or after more removes) by adds, the
 *  i-th remove pairs with the i-th add. Leftover removes pad the right
 *  column with empty cells; leftover adds pad the left. Context lines
 *  pair with themselves. `lines_highlighted` (aligned 1:1 with `lines`
 *  by the backend, see `highlight_group_lines`) is threaded through so
 *  both panes keep the server-side syntect tokens. */
export function alignHunkRows(hunk: DiffHunk): SplitRow[] {
  const rows: SplitRow[] = [];
  const removes: { line: DiffLine; html?: string }[] = [];

  const flushRemoves = (upTo: number) => {
    // Emit remove-only rows for any removes not yet paired with an add.
    for (let i = upTo; i < removes.length; i++) {
      rows.push({ old: removes[i].line, new: null, oldHtml: removes[i].html });
    }
    removes.length = 0;
  };

  hunk.lines.forEach((line, i) => {
    const html = hunk.lines_highlighted?.[i];
    if (line.line_type === 'remove') {
      removes.push({ line, html });
      return;
    }
    if (line.line_type === 'add') {
      // Pair with the oldest unpaired remove, if any (GitHub alignment).
      const partner = removes.shift();
      if (partner) {
        rows.push({ old: partner.line, new: line, oldHtml: partner.html, newHtml: html });
      } else {
        rows.push({ old: null, new: line, newHtml: html });
      }
      return;
    }
    // Context (or a no-newline terminator row): flush any unpaired removes
    // first so a remove-run followed by context doesn't visually merge the
    // removal with the context row, then emit the context as a full row.
    flushRemoves(0);
    rows.push({ old: line, new: line, oldHtml: html, newHtml: html });
  });

  // Trailing removes with no add partner.
  flushRemoves(0);
  return rows;
}

/** Split a path into its (dimmed) directory and (emphasised) filename. */
export function splitPath(path: string): { dir: string; name: string } {
  const idx = path.lastIndexOf('/');
  if (idx === -1) return { dir: '', name: path };
  return { dir: path.slice(0, idx + 1), name: path.slice(idx + 1) };
}

/** Match `@@ -old[,old_lines] +new[,new_lines] @@`. The count portion is
 *  optional in unified diff — when absent it defaults to 1. */
const HUNK_HEADER_RE = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/;

/** A raw unified-diff line, classified. The `\\ No newline at end of file`
 *  terminator gets its own kind so the renderer can dim it instead of
 *  treating it like a context row. */
export type PatchLineKind = 'add' | 'remove' | 'context' | 'no_newline';

export interface ClassifiedPatchLine {
  kind: PatchLineKind;
  /** The line's content WITHOUT the leading `+`/`-`/` `/`\\` marker. */
  content: string;
}

export function classifyPatchLine(raw: string): ClassifiedPatchLine {
  if (raw.startsWith('@@')) {
    // Hunk header — the caller handles rendering this separately.
    return { kind: 'context', content: raw };
  }
  if (raw.startsWith('+')) return { kind: 'add', content: raw.slice(1) };
  if (raw.startsWith('-')) return { kind: 'remove', content: raw.slice(1) };
  if (raw.startsWith('\\')) {
    // `\ No newline at end of file` — render as a dimmed "no newline" row.
    return { kind: 'no_newline', content: raw };
  }
  return { kind: 'context', content: raw.startsWith(' ') ? raw.slice(1) : raw };
}

/** Parse a GitHub-style unified diff (`patch` field on `PrFileEntry`) into
 *  the `DiffHunk[]` shape `<HunkBlock>` already understands. Tracks running
 *  `old_num` / `new_num` counters per hunk so the unified review surface's
 *  line-number gutters light up correctly for PR diffs too (#725). */
export function parsePatchIntoHunks(patch: string): DiffHunk[] {
  const lines = patch.split('\n');
  if (lines.length > 0 && lines[lines.length - 1] === '') lines.pop();

  const hunks: DiffHunk[] = [];
  let current: DiffHunk | null = null;
  let oldNum = 0;
  let newNum = 0;

  for (const raw of lines) {
    const header = HUNK_HEADER_RE.exec(raw);
    if (header) {
      // Push the previous hunk (if any) and start a new one.
      if (current) hunks.push(current);
      const oldStart = Number(header[1]);
      const oldLines = header[2] === undefined ? 1 : Number(header[2]);
      const newStart = Number(header[3]);
      const newLines = header[4] === undefined ? 1 : Number(header[4]);
      oldNum = oldStart;
      newNum = newStart;
      current = {
        old_start: oldStart,
        old_lines: oldLines,
        new_start: newStart,
        new_lines: newLines,
        old_highlighted: '',
        new_highlighted: '',
        lines: [],
        lines_highlighted: [],
      };
      continue;
    }

    // Stray lines outside a hunk (e.g. a leading comment) are dropped —
    // matches GitHub's own parser, which treats them as non-diff noise.
    if (!current) continue;

    const { kind, content } = classifyPatchLine(raw);
    switch (kind) {
      case 'add':
        current.lines.push({
          line_type: 'add',
          content,
          old_num: null,
          new_num: newNum,
        });
        newNum++;
        break;
      case 'remove':
        current.lines.push({
          line_type: 'remove',
          content,
          old_num: oldNum,
          new_num: null,
        });
        oldNum++;
        break;
      case 'context':
        current.lines.push({
          line_type: 'context',
          content,
          old_num: oldNum,
          new_num: newNum,
        });
        oldNum++;
        newNum++;
        break;
      case 'no_newline':
        // Don't bump counters — these aren't real source lines.
        current.lines.push({
          line_type: 'context',
          content,
          old_num: null,
          new_num: null,
        });
        break;
    }
  }
  if (current) hunks.push(current);
  return hunks;
}
