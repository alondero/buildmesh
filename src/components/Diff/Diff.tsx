import { useState } from 'react';
import type { DiffHunk, DiffLine, FileDiff, FileDiffStatus } from '../../lib/tauri';

/**
 * Shared diff renderer: a GitHub-style stacked, unified, syntax-highlighted
 * view. One `<FileDiffCard>` per changed file, each collapsible, with a hunk
 * header (`@@ … @@`) between change regions and per-line highlighting fed by
 * the backend's `lines_highlighted`. Used by the desktop review panel; mobile
 * keeps its own layout.
 */

interface StatusMeta {
  letter: string;
  label: string;
  /** Tailwind text colour token for the badge. */
  color: string;
}

// Mirrors the status vocabulary shared with `GitStatus.status`.
const STATUS_META: Record<FileDiffStatus, StatusMeta> = {
  added: { letter: 'A', label: 'Added', color: 'text-accent-green' },
  modified: { letter: 'M', label: 'Modified', color: 'text-accent-amber' },
  deleted: { letter: 'D', label: 'Deleted', color: 'text-accent-red' },
  renamed: { letter: 'R', label: 'Renamed', color: 'text-purple-400' },
  untracked: { letter: '?', label: 'Untracked', color: 'text-text-muted' },
};

export function statusMeta(status: FileDiffStatus | ''): StatusMeta {
  return STATUS_META[status as FileDiffStatus] ?? STATUS_META.modified;
}

/** Split a path into its (dimmed) directory and (emphasised) filename. */
function splitPath(path: string): { dir: string; name: string } {
  const idx = path.lastIndexOf('/');
  if (idx === -1) return { dir: '', name: path };
  return { dir: path.slice(0, idx + 1), name: path.slice(idx + 1) };
}

/** A stable DOM id so a file-list can scroll a card into view. */
export function diffCardId(path: string): string {
  return `diff-file-${path}`;
}

function lineClasses(type: DiffLine['line_type']): string {
  switch (type) {
    case 'add':
      return 'bg-accent-green/15';
    case 'remove':
      return 'bg-accent-red/15';
    default:
      return '';
  }
}

function marker(type: DiffLine['line_type']): string {
  return type === 'add' ? '+' : type === 'remove' ? '-' : ' ';
}

function DiffLineRow({ line, html }: { line: DiffLine; html?: string }) {
  return (
    <div className={`flex ${lineClasses(line.line_type)}`}>
      <span className="w-10 px-1 text-right text-text-muted/50 select-none flex-shrink-0">
        {line.old_num ?? ''}
      </span>
      <span className="w-10 px-1 text-right text-text-muted/50 select-none flex-shrink-0">
        {line.new_num ?? ''}
      </span>
      <span
        className={`w-4 text-center select-none flex-shrink-0 ${
          line.line_type === 'add'
            ? 'text-accent-green'
            : line.line_type === 'remove'
              ? 'text-accent-red'
              : 'text-text-muted/40'
        }`}
      >
        {marker(line.line_type)}
      </span>
      {html ? (
        // Highlighted HTML is generated server-side by syntect (trusted, not
        // user-controlled markup), one span run per line.
        <span
          className="whitespace-pre flex-1"
          dangerouslySetInnerHTML={{ __html: html }}
        />
      ) : (
        <span className="whitespace-pre flex-1">{line.content}</span>
      )}
    </div>
  );
}

function HunkBlock({ hunk, last }: { hunk: DiffHunk; last: boolean }) {
  return (
    <div className={last ? '' : 'border-b border-border-subtle'}>
      <div className="px-2 py-0.5 bg-bg-overlay/60 text-text-muted select-none">
        @@ -{hunk.old_start},{hunk.old_lines} +{hunk.new_start},{hunk.new_lines} @@
      </div>
      {hunk.lines.map((line, i) => (
        <DiffLineRow key={i} line={line} html={hunk.lines_highlighted?.[i]} />
      ))}
    </div>
  );
}

export function FileDiffCard({
  file,
  defaultOpen = true,
}: {
  file: FileDiff;
  defaultOpen?: boolean;
}) {
  const [open, setOpen] = useState(defaultOpen);
  const meta = statusMeta(file.status);
  const { dir, name } = splitPath(file.path);

  return (
    <div
      id={diffCardId(file.path)}
      className="border-b border-border-subtle scroll-mt-1"
    >
      {/* Header — sticks to the top of the scroll area while the card is in view */}
      <button
        onClick={() => setOpen((o) => !o)}
        className="sticky top-0 z-10 w-full flex items-center gap-2 px-2 py-1.5 bg-bg-surface hover:bg-bg-card transition-colors text-left border-b border-border-subtle"
      >
        <span className="text-text-muted w-3 text-center text-[10px] flex-shrink-0">
          {open ? '▼' : '▶'}
        </span>
        <span
          className={`font-bold w-3 flex-shrink-0 ${meta.color}`}
          title={meta.label}
        >
          {meta.letter}
        </span>
        <span className="flex-1 truncate font-mono text-[11px] min-w-0">
          {file.old_path && file.old_path !== file.path && (
            <span className="text-text-muted">
              {file.old_path} <span aria-hidden="true">→</span>{' '}
            </span>
          )}
          <span className="text-text-muted">{dir}</span>
          <span className="text-text-primary">{name}</span>
        </span>
        {file.additions > 0 && (
          <span className="text-accent-green flex-shrink-0 font-mono text-[11px]">
            +{file.additions}
          </span>
        )}
        {file.deletions > 0 && (
          <span className="text-accent-red flex-shrink-0 font-mono text-[11px]">
            -{file.deletions}
          </span>
        )}
      </button>

      {open && (
        <div className="font-mono text-xs leading-5">
          {file.binary ? (
            <div className="px-3 py-2 text-text-muted italic">
              Binary file not shown
            </div>
          ) : file.hunks.length === 0 ? (
            <div className="px-3 py-2 text-text-muted italic">No changes</div>
          ) : (
            file.hunks.map((hunk, i) => (
              <HunkBlock
                key={i}
                hunk={hunk}
                last={i === file.hunks.length - 1}
              />
            ))
          )}
        </div>
      )}
    </div>
  );
}

export interface DiffTotals {
  files: number;
  additions: number;
  deletions: number;
}

export function diffTotals(files: FileDiff[]): DiffTotals {
  return files.reduce<DiffTotals>(
    (acc, f) => ({
      files: acc.files + 1,
      additions: acc.additions + f.additions,
      deletions: acc.deletions + f.deletions,
    }),
    { files: 0, additions: 0, deletions: 0 }
  );
}

/** Stacked review surface: every changed file's diff in one scroll column. */
export function Diff({ files }: { files: FileDiff[] }) {
  if (files.length === 0) {
    return (
      <div className="flex items-center justify-center h-full text-text-muted text-xs">
        No changes
      </div>
    );
  }
  return (
    <div>
      {files.map((file) => (
        <FileDiffCard key={file.path} file={file} />
      ))}
    </div>
  );
}
