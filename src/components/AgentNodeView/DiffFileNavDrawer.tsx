/**
 * DiffFileNavDrawer — the collapsible Quick File Navigation Drawer for
 * issue #1374 (item 3 of the acceptance criteria).
 *
 * Sits inside the center diff overlay's body as a left-edge side panel
 * that lists every changed file in the active changeset — base diffs
 * pull from `node_changed_files`, head diffs pull from `get_git_status`.
 * Each row carries the M/A/D badge (`fileDiffStatusMeta` is the shared
 * vocabulary) and the +/- line counts, and clicking a row jumps the
 * overlay's body to that file via the `onSelectFile` callback.
 *
 * The drawer is closed by default so the diff gets the full center
 * workspace on first paint; users who want to hop between files
 * without returning to the Probe's right-hand panel toggle it on.
 *
 * Fetch lifecycle is one-shot on mount + when `fetchFiles` changes
 * (i.e. when the user switches between a base and a head diff, or
 * between two nodes). It does NOT auto-refresh on `git-changed` —
 * the overlay's existing background-refresh path covers the diff
 * body, and the file list rarely changes shape while reviewing.
 * Re-open the drawer to refresh.
 */
import { useEffect, useState } from 'react';
import { formatError } from '../../lib/errorUtils';
import type { GitStatus } from '../../lib/tauri';
import { fileDiffStatusMeta } from '../../lib/status';
import { LoadingState } from '../shared/Spinner';
import { splitPath } from '../Diff/diffFormat';

export interface DiffFileNavDrawerProps {
  /** Bulk file-list endpoint. Base diffs use the agent-node endpoint,
   *  head diffs use the worktree-status endpoint — the parent picks
   *  based on `diff.source` and the file list shape is identical. */
  fetchFiles: () => Promise<GitStatus[]>;
  /** Currently-viewed file path — highlighted in the list so the user
   *  can see at a glance which file the diff body is rendering. */
  currentFilePath: string;
  /** Called when the user clicks a row. The parent updates
   *  `activeDiffFile` in the UI store so the diff body re-fetches. */
  onSelectFile: (path: string) => void;
  /** Bumped by the parent after a successful Stage/Revert so the
   *  drawer refetches (a counter the parent increments rather than a
   *  state setter avoids the re-render churn of passing a callback
   *  that captures `files`). Issue #1374 review feedback — without
   *  this, the drawer's M/A/D badges went stale after every
   *  quick action. */
  refreshKey: number;
}

export function DiffFileNavDrawer({
  fetchFiles,
  currentFilePath,
  onSelectFile,
  refreshKey,
}: DiffFileNavDrawerProps) {
  const [files, setFiles] = useState<GitStatus[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  // One-shot fetch on mount + whenever the parent swaps `fetchFiles`
  // (i.e. lens changes: base→head, mesh A→mesh B, node X→node Y) OR
  // bumps `refreshKey` (parent re-staged or reverted a file). The
  // previous request is "cancelled" by the `cancelled` flag — a stale
  // `.then` from an earlier fetch can't overwrite a newer one (same
  // pattern as `CenterHeadBaseDiff.fetchDiff`, see #1181).
  useEffect(() => {
    let cancelled = false;
    setFiles(null);
    setError(null);
    fetchFiles()
      .then((list) => {
        if (cancelled) return;
        setFiles(list);
      })
      .catch((e) => {
        if (cancelled) return;
        setError(formatError(e));
      });
    return () => {
      cancelled = true;
    };
  }, [fetchFiles, refreshKey]);

  return (
    <aside
      data-testid="diff-file-nav"
      aria-label="Changed files in this changeset"
      className="w-64 shrink-0 border-r border-border-subtle bg-bg-surface flex flex-col min-h-0"
    >
      <div className="px-3 py-2 border-b border-border-subtle text-2xs uppercase tracking-wide text-text-muted font-medium">
        Changed files
      </div>
      <div className="flex-1 min-h-0 overflow-y-auto">
        {error ? (
          <div
            role="alert"
            data-testid="diff-file-nav-error"
            className="px-3 py-2 text-2xs text-accent-red"
          >
            {error}
          </div>
        ) : files === null ? (
          <div className="flex items-center justify-center py-6">
            <LoadingState label="Loading files…" />
          </div>
        ) : files.length === 0 ? (
          <div
            data-testid="diff-file-nav-empty"
            className="px-3 py-2 text-2xs text-text-muted italic"
          >
            No changes
          </div>
        ) : (
          <ul className="divide-y divide-border-subtle">
            {files.map((f) => (
              <DrawerRow
                key={f.path}
                file={f}
                isCurrent={f.path === currentFilePath}
                onSelect={() => onSelectFile(f.path)}
              />
            ))}
          </ul>
        )}
      </div>
    </aside>
  );
}

/** One file row in the drawer. The badge reuses the shared
 *  `fileDiffStatusMeta` so the M/A/D colour scheme matches the file
 *  cards in the diff body and the Probe's Project Files tree. */
function DrawerRow({
  file,
  isCurrent,
  onSelect,
}: {
  file: GitStatus;
  isCurrent: boolean;
  onSelect: () => void;
}) {
  const meta = fileDiffStatusMeta(file.status);
  const { dir, name } = splitPath(file.path);
  return (
    <li>
      <button
        type="button"
        onClick={onSelect}
        data-testid="diff-file-nav-row"
        data-path={file.path}
        aria-current={isCurrent ? 'true' : undefined}
        className={`w-full text-left px-3 py-1.5 flex items-center gap-2 text-2xs font-mono transition-colors ${
          isCurrent
            ? 'bg-accent-cyan/10 text-text-primary'
            : 'hover:bg-bg-card text-text-secondary'
        }`}
      >
        <span
          aria-label={meta.label}
          title={meta.label}
          className={`font-bold w-3 text-center flex-shrink-0 ${meta.color}`}
        >
          {meta.letter}
        </span>
        <span className="flex-1 truncate min-w-0" title={file.path}>
          {dir && <span className="text-text-muted">{dir}</span>}
          <span className="text-text-primary">{name}</span>
        </span>
        {file.additions > 0 && (
          <span className="text-accent-green flex-shrink-0">+{file.additions}</span>
        )}
        {file.deletions > 0 && (
          <span className="text-accent-red flex-shrink-0">-{file.deletions}</span>
        )}
      </button>
    </li>
  );
}
