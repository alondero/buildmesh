import type { GitStatus } from '../../lib/tauri';
import type { FileDiffStatusMeta } from '../../lib/status';

interface ChangedFileRowProps {
  /** One uncommitted file, as `get_git_status` reports it. */
  file: GitStatus;
  /** `fileDiffStatusMeta(file.status)` — the caller owns it so this stays a
   *  pure span sequence and the row wrapper stays at the call site
   *  (issue #2406). */
  meta: FileDiffStatusMeta;
}

/**
 * The contents of one changed-file row: status letter, path, and the
 * `+additions / -deletions` counts.
 *
 * `ChangedFilesSection` wraps it in a clickable `<button>`; the
 * worktree-close dialog wraps it in a plain `<div>`. The two lists used to
 * carry this markup byte-for-byte each, so they could drift; the wrapper
 * differs (interaction, hover, selection highlight) and stays with the caller.
 * Returns a fragment deliberately — any element here would change the
 * rendered output of both call sites.
 */
export function ChangedFileRow({ file, meta }: ChangedFileRowProps) {
  return (
    <>
      <span className={`font-bold w-3 flex-shrink-0 ${meta.color}`} title={meta.label}>
        {meta.letter}
      </span>
      <span className="flex-1 truncate text-text-secondary">{file.path}</span>
      <span className="text-accent-green flex-shrink-0">+{file.additions}</span>
      <span className="text-accent-red flex-shrink-0">-{file.deletions}</span>
    </>
  );
}
