import { fileDiffStatusMeta } from '../../lib/status';
import type { GitStatus } from '../../types/generated/GitStatus';

/**
 * Inner row markup shared by `ChangedFilesSection` (a clickable button with a
 * selected-row highlight) and `WorktreeCloseDialog` (a read-only div listing
 * what "Remove anyway" would discard). The two wrappers diverged in PR #790
 * — the button adds hover, selection, and chevron-aware padding — but the
 * four spans for status letter / path / `+additions` / `-deletions` were
 * byte-for-byte identical. Issue #791 consolidates the inner row only; the
 * wrapper (button vs. div, highlight, onClick) stays with each caller.
 *
 * Renders a Fragment of the four spans — no extra DOM node — so the caller's
 * flex container continues to own `gap-2` / `px-2` / `py-0.5` and the
 * `flex-1` / `flex-shrink-0` children keep their layout intact.
 */
export interface ChangedFileRowProps {
  file: GitStatus;
}

export function ChangedFileRow({ file }: ChangedFileRowProps) {
  const meta = fileDiffStatusMeta(file.status);
  return (
    <>
      <span
        className={`font-bold w-3 flex-shrink-0 ${meta.color}`}
        title={meta.label}
      >
        {meta.letter}
      </span>
      <span className="flex-1 truncate text-text-secondary">{file.path}</span>
      <span className="text-accent-green flex-shrink-0">+{file.additions}</span>
      <span className="text-accent-red flex-shrink-0">-{file.deletions}</span>
    </>
  );
}