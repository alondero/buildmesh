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
 */

import { useCallback, useEffect, useRef, useState } from 'react';
import { getPrFiles, type PrFileEntry } from '../../lib/tauri';
import { useUIStore, type DiffContext } from '../../stores/uiStore';
import { splitPath } from '../Diff/diffFormat';
import { LoadingState } from '../shared/Spinner';

interface PrDiffViewProps {
  /** The PR lens — `prNumber` and `filePath` (`''` for list view) drive the
   *  fetch / render. `meshId` is preserved by the caller so the overlay's
   *  auto-close (different mesh selected) still works. */
  diff: DiffContext;
}

const STATUS_META: Record<string, { letter: string; color: string }> = {
  added: { letter: 'A', color: 'text-accent-green' },
  modified: { letter: 'M', color: 'text-accent-amber' },
  deleted: { letter: 'D', color: 'text-accent-red' },
  renamed: { letter: 'R', color: 'text-accent-violet' },
};

function statusMeta(status: string) {
  // GitHub returns a small fixed set; anything else (e.g. "copied" /
  // "changed" / "unchanged") surfaces as a muted "modified"-style badge
  // rather than rendering blank.
  return STATUS_META[status] ?? { letter: 'M', color: 'text-text-muted' };
}

type PatchLineKind = 'add' | 'remove' | 'context' | 'hunk';

/** Classify one line of a unified diff (the `+`/`-`/` ` convention). The
 *  `@@` hunk header gets its own kind so we can render it in the muted
 *  hunk-header style instead of mistaking it for an empty context line. */
function classifyPatchLine(raw: string): { kind: PatchLineKind; content: string } {
  if (raw.startsWith('@@')) return { kind: 'hunk', content: raw };
  if (raw.startsWith('+')) return { kind: 'add', content: raw.slice(1) };
  if (raw.startsWith('-')) return { kind: 'remove', content: raw.slice(1) };
  if (raw.startsWith('\\')) {
    // `\ No newline at end of file` — GitHub's terminator marker, very
    // common. Render as a dimmed "no newline" line rather than a context row.
    return { kind: 'context', content: raw };
  }
  return { kind: 'context', content: raw.startsWith(' ') ? raw.slice(1) : raw };
}

export function PrDiffView({ diff }: PrDiffViewProps) {
  const openDiff = useUIStore((s) => s.openDiff);
  const prNumber = diff.prNumber;

  // Defensive: callers should only mount us with `source === 'pr'`, but a
  // missing prNumber is a programming error worth surfacing instead of
  // silently rendering empty state.
  if (prNumber === undefined) {
    return (
      <div className="h-full flex items-center justify-center text-accent-red text-xs px-3 text-center">
        PR diff opened without a prNumber
      </div>
    );
  }

  const [files, setFiles] = useState<PrFileEntry[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Monotonic token — identical pattern to CenterDiffOverlay / AgentReviewPanel.
  // A rapid `setActiveDiffFile` switch (e.g. list → file) must not let an
  // older fetch's resolution overwrite the new state.
  const reqId = useRef(0);

  const fetchFiles = useCallback(() => {
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
        // `e.message` is what we want to render — `String(e)` would prepend
        // "Error: " (because `e` is an Error), which the central IPC
        // chokepoint (#386) unwraps before rethrowing.
        setError(e instanceof Error ? e.message : String(e));
      });
  }, [diff.meshId, prNumber]);

  useEffect(() => {
    fetchFiles();
  }, [fetchFiles]);

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
          const meta = statusMeta(file.status);
          const { dir, name } = splitPath(file.filename);
          return (
            <button
              key={file.filename}
              type="button"
              onClick={() => onSelectFile(file.filename)}
              className="w-full flex items-center gap-2 px-3 py-1.5 bg-bg-surface hover:bg-bg-card transition-colors text-left border-b border-border-subtle"
            >
              <span className={`font-bold w-3 flex-shrink-0 ${meta.color}`} title={file.status}>
                {meta.letter}
              </span>
              <span className="flex-1 truncate font-mono text-xs min-w-0">
                {file.previous_filename && file.previous_filename !== file.filename && (
                  <span className="text-text-muted">
                    {file.previous_filename} <span aria-hidden="true">→</span>{' '}
                  </span>
                )}
                <span className="text-text-muted">{dir}</span>
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
 *  GitHub's unified diff text. No syntect here (we don't ship the syntax
 *  highlighter to the frontend), but the layout matches the existing
 *  `<DiffLineRow>` for a familiar feel. */
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

  const lines = file.patch.split('\n');
  // GitHub patches often end with a trailing newline, which produces an
  // extra empty string after the split. Drop it so we don't render a blank
  // trailing row.
  if (lines.length > 0 && lines[lines.length - 1] === '') lines.pop();

  return (
    <div className="font-mono text-xs leading-5">
      {lines.map((raw, i) => {
        const { kind, content } = classifyPatchLine(raw);
        if (kind === 'hunk') {
          return (
            <div
              key={i}
              className="px-2 py-0.5 bg-bg-overlay/60 text-text-muted select-none"
            >
              {content}
            </div>
          );
        }
        const bg =
          kind === 'add'
            ? 'bg-accent-green/15'
            : kind === 'remove'
              ? 'bg-accent-red/15'
              : '';
        const markerColor =
          kind === 'add'
            ? 'text-accent-green'
            : kind === 'remove'
              ? 'text-accent-red'
              : 'text-text-muted/40';
        const markerChar = kind === 'add' ? '+' : kind === 'remove' ? '-' : ' ';
        return (
          <div key={i} className={`flex ${bg}`}>
            <span className={`w-4 text-center select-none flex-shrink-0 ${markerColor}`}>
              {markerChar}
            </span>
            <span className="whitespace-pre flex-1 px-2">{content}</span>
          </div>
        );
      })}
    </div>
  );
}
