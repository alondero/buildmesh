/**
 * PrDiffView — issue #421.
 *
 * Renders a GitHub pull request's diff inside the Center Workspace Diff
 * Overlay (the surface #379 introduced for `source: 'head' | 'base'`). Has
 * two modes, switched by `diff.filePath`:
 *
 *   - **List view** (`filePath === ''`): the PR's whole file list, each row
 *     clickable to drill into a single-file diff. Replaces the
 *     "click a file in the narrow 360px body" pattern with a generous
 *     center-workspace view.
 *   - **File view** (`filePath !== ''`): that one file's unified `patch`
 *     text, classified +/−/context line-by-line.
 *
 * The data comes from `GET /repos/{o}/{r}/pulls/{n}/files` via the
 * `get_pr_files` Tauri command, so the diff is independent of the local
 * repo state — the PR's head branch may not even exist locally, and that's
 * fine. We never call `useGitPathInvalidation` because there is no local
 * file watcher for a remote PR; the user gets a snapshot, like github.com.
 *
 * The component owns its own fetch + loading state because the
 * CenterDiffOverlay parent is intentionally source-agnostic — it sees
 * `DiffContext` and just routes.
 *
 * Issue #725: the patch body now flows through the same `<HunkBlock>` that
 * the desktop review surface uses (via `parsePatchIntoHunks`), so the
 * gutter widths and `/15` accent-fill opacity are uniform across all diff
 * surfaces.
 */

import { formatError } from '../../lib/errorUtils';
import { useCallback, useEffect, useRef, useState } from 'react';
import { getPrFiles, type PrFileEntry } from '../../lib/tauri';
import { useUIStore, type DiffContext } from '../../stores/uiStore';
import { fileDiffStatusMeta } from '../../lib/status';
import { splitPath, parsePatchIntoHunks } from '../Diff/diffFormat';
import { HunkBlock } from '../Diff/Diff';
import { LoadingState } from '../shared/Spinner';

interface PrDiffViewProps {
  /** The PR lens — `prNumber` and `filePath` (`''` for list view) drive the
   *  fetch / render. `meshId` is preserved by the caller so the overlay's
   *  auto-close (different mesh selected) still works. */
  diff: DiffContext;
}

export function PrDiffView({ diff }: PrDiffViewProps) {
  const openDiff = useUIStore((s) => s.openDiff);
  const prNumber = diff.prNumber;

  // Issue #1242: every hook below runs unconditionally, regardless of
  // `prNumber`. The earlier shape had an early `return` for the
  // prNumber-undefined case BEFORE the hook declarations, which is a
  // Rules-of-Hooks violation — React records the hook count per fiber
  // and a later render that calls a different number of hooks throws
  // "Rendered more hooks than during the previous render" and trips the
  // workspace-wide ErrorBoundary. Today the placeholder text stays put
  // for any mounted overlay that ever sees prNumber===undefined (a
  // malformed `openDiff({source:'pr'})`, or a future refactor that
  // flips the lens), so the defensive guard must live BELOW the hooks
  // and decide what to *render*, not whether to *run*.

  const [files, setFiles] = useState<PrFileEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Monotonic token — identical pattern to CenterDiffOverlay / AgentReviewPanel.
  // A rapid `setActiveDiffFile` switch (e.g. list → file) must not let an
  // older fetch's resolution overwrite the new state.
  const reqId = useRef(0);

  const fetchFiles = useCallback(() => {
    // Defensive: callers should only mount us with `source === 'pr'`,
    // but a missing prNumber is a programming error worth surfacing
    // instead of silently rendering empty state. No-op the fetch — the
    // placeholder JSX below explains why — rather than throwing into a
    // then/catch that would mask the real problem.
    if (prNumber === undefined) return;
    const myId = ++reqId.current;
    setFiles(null);
    setError(null);
    getPrFiles(diff.meshId, prNumber)
      .then((result) => {
        if (reqId.current !== myId) return;
        setFiles(result);
      })
      .catch((e) => {
        if (reqId.current !== myId) return;
        setError(formatError(e));
      });
  }, [diff.meshId, prNumber]);

  useEffect(() => {
    fetchFiles();
  }, [fetchFiles]);

  // Now — and only now — is it safe to short-circuit on prNumber===undefined.
  // The hooks above have already run, so this branch renders the
  // placeholder without changing the fiber's hook count for the next
  // render.
  if (prNumber === undefined) {
    return (
      <div className="h-full flex items-center justify-center text-accent-red text-xs px-3 text-center">
        PR diff opened without a prNumber
      </div>
    );
  }

  if (error) {
    return (
      <div className="h-full flex items-center justify-center text-accent-red text-xs px-3 text-center">
        {error}
      </div>
    );
  }
  if (files === null) {
    return (
      <div className="h-full flex items-center justify-center">
        <LoadingState label="Loading PR…" />
      </div>
    );
  }

  // ----- List view -------------------------------------------------------
  if (diff.filePath === '') {
    if (files.length === 0) {
      return (
        <div className="h-full flex items-center justify-center text-text-muted text-xs">
          No files changed in this PR
        </div>
      );
    }
    return (
      <PrFileList
        files={files}
        prNumber={prNumber}
        onSelectFile={(filename) =>
          openDiff({ ...diff, filePath: filename })
        }
      />
    );
  }

  // ----- File view -------------------------------------------------------
  const file = files.find((f) => f.filename === diff.filePath);
  if (!file) {
    return (
      <div className="h-full flex items-center justify-center text-text-muted text-xs px-3 text-center">
        File {diff.filePath} not in this PR
      </div>
    );
  }
  return <PrPatch file={file} />;
}

/** PR file list — the overlay's "list view" mode. Each row mirrors the
 *  sticky-header card style of the existing `<FileDiffCard>` so users
 *  recognise it from the review tab. Click drills in. */
function PrFileList({
  files,
  prNumber,
  onSelectFile,
}: {
  files: PrFileEntry[];
  prNumber: number;
  onSelectFile: (filename: string) => void;
}) {
  const totals = files.reduce(
    (acc, f) => ({
      additions: acc.additions + f.additions,
      deletions: acc.deletions + f.deletions,
    }),
    { additions: 0, deletions: 0 },
  );
  return (
    <div className="min-h-0">
      {/* Summary bar — same shape as AgentReviewPanel's. "vs base" would be
       *  a lie here (we're diffing head against the PR's base, but
       *  "vs base" overloads the term with the local merge-base meaning);
       *  "PR #N" is unambiguous and matches the chip users clicked. */}
      <div className="sticky top-0 z-20 flex items-center gap-2 px-3 py-1.5 bg-bg-overlay border-b border-border-subtle text-xs">
        <span className="text-text-secondary font-medium">
          {files.length} {files.length === 1 ? 'file' : 'files'} in PR #{prNumber}
        </span>
        {totals.additions > 0 && (
          <span className="text-accent-green font-mono">+{totals.additions}</span>
        )}
        {totals.deletions > 0 && (
          <span className="text-accent-red font-mono">-{totals.deletions}</span>
        )}
        <span
          className="ml-auto text-text-muted"
          title="Files changed in this pull request"
        >
          PR diff
        </span>
      </div>

      <div>
        {files.map((file) => {
          const meta = fileDiffStatusMeta(file.status);
          const { dir, name } = splitPath(file.filename);
          return (
            <button
              key={file.filename}
              type="button"
              onClick={() => onSelectFile(file.filename)}
              className="w-full flex items-center gap-2 px-3 py-1.5 bg-bg-surface hover:bg-bg-card transition-colors text-left border-b border-border-subtle"
            >
              <span className={`font-bold w-3 flex-shrink-0 ${meta.color}`} title={meta.label}>
                {meta.letter}
              </span>
              <span className="flex-1 truncate font-mono text-xs min-w-0">
                {file.previous_filename && file.previous_filename !== file.filename && (
                  <span className="text-text-secondary">
                    {file.previous_filename} <span aria-hidden="true">→</span>{' '}
                  </span>
                )}
                <span className="text-text-secondary">{dir}</span>
                <span className="text-text-primary">{name}</span>
              </span>
              {file.additions > 0 && (
                <span className="text-accent-green flex-shrink-0 font-mono text-xs">
                  +{file.additions}
                </span>
              )}
              {file.deletions > 0 && (
                <span className="text-accent-red flex-shrink-0 font-mono text-xs">
                  -{file.deletions}
                </span>
              )}
            </button>
          );
        })}
      </div>
    </div>
  );
}

/** Single-file patch view — line-by-line +/−/context rendering of
 *  GitHub's unified diff text. Issue #725: the body now flows through the
 *  shared `<HunkBlock>` from `<Diff>`, so PR diffs pick up the canonical
 *  `w-10` gutters and `/15` accent-fill opacity the desktop review surface
 *  uses (the previous bespoke renderer used just a `w-4` marker column). */
function PrPatch({ file }: { file: PrFileEntry }) {
  // A binary file: GitHub omits `patch` (we send "" via #[serde(default)]).
  // Render an "Binary file" placeholder rather than a blank pane.
  if (file.patch === '') {
    return (
      <div className="h-full flex items-center justify-center text-text-muted text-xs italic px-3 text-center">
        Binary file not shown
      </div>
    );
  }

  const hunks = parsePatchIntoHunks(file.patch);

  return (
    <div className="font-mono text-xs leading-5">
      {hunks.map((hunk, i) => (
        <HunkBlock key={i} hunk={hunk} last={i === hunks.length - 1} />
      ))}
    </div>
  );
}
