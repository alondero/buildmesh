/**
 * Shared diff formatting helpers — extracted from `Diff.tsx` and
 * `PrDiffView.tsx` to avoid two-place edits when a path-split rule
 * changes (e.g. Windows separators per issue #241, or a future rename).
 * `splitPath` was byte-identical in both files; this is its only home.
 *
 * Status meta: the two surfaces have intentionally diverged on
 * unknown-status fallbacks (PrDiffView mutes; Diff.tsx tints amber as a
 * normal modification) and on the `untracked` key / `label` field that
 * PrDiffView doesn't need. Keep `STATUS_META` per-surface until a third
 * caller forces a single product decision; don't extract a shared one
 * for its own sake.
 */

/** Split a path into its (dimmed) directory and (emphasised) filename. */
export function splitPath(path: string): { dir: string; name: string } {
  const idx = path.lastIndexOf('/');
  if (idx === -1) return { dir: '', name: path };
  return { dir: path.slice(0, idx + 1), name: path.slice(idx + 1) };
}
