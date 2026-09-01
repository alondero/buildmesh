import { useState, useSyncExternalStore } from 'react';
import type { DiffHunk, DiffLine, FileDiff } from '../../lib/tauri';
import { fileDiffStatusMeta } from '../../lib/status';
import { splitPath, alignHunkRows, type SplitRow } from './diffFormat';

/** Where the Unified/Split preference is saved (issue #1374). */
export const DIFF_VIEW_STORAGE_KEY = 'buildmesh.diff-view';
export type DiffViewMode = 'unified' | 'split';

/** Boot the Unified/Split preference from localStorage, try/catch-wrapped
 *  like `loadViewMode` in `uiStore` (storage unavailable in tests / private
 *  mode). Anything but 'split' falls back to 'unified'. */
export function loadDiffViewMode(): DiffViewMode {
  try {
    const stored = localStorage.getItem(DIFF_VIEW_STORAGE_KEY);
    if (stored === 'split') return 'split';
  } catch {
    // localStorage unavailable — default to unified.
  }
  return 'unified';
}

/** Write-on-change persistence, try/catch-wrapped like `persistViewMode` —
 *  a storage failure must never block the toggle itself. */
export function persistDiffViewMode(mode: DiffViewMode): void {
  try {
    localStorage.setItem(DIFF_VIEW_STORAGE_KEY, mode);
  } catch {
    // localStorage unavailable — in-memory state still flips.
  }
}

// ─── Issue #1374 — single source of truth for the Unified/Split mode ──────
//
// The earlier shape (one `useState(loadDiffViewMode)` per component, plus a
// separate one in `DiffOverlayShell`) had three layers fighting over
// localStorage — every toggle fired two writes (the card's localStorage
// call AND the shell's), and the inline `FileDiffCard` toggle had to call
// `onModeChange` AND update its own dead `ownMode`. The fix:
//
//   • One module-level `useSyncExternalStore`-backed store (singleton).
//   • `useDiffViewMode()` is THE hook — every caller reads/writes via it,
//     so the shell, the stacked-review `Diff`, and the inline `FileDiffCard`
//     toggle all stay in sync without prop-drilling.
//   • The `mode`/`onModeChange` props on `Diff`/`FileDiffCard` are
//     OPTIONAL controlled overrides — when supplied (overlay case), the
//     shell remains the single source; when omitted (stacked review
//     case), the card falls back to the hook.
//
// `DiffOverlayShell` is now a thin pass-through: `const [mode, setMode] =
// useDiffViewMode()`, then `<Diff mode={mode} onModeChange={setMode} />`.

type Listener = () => void;
let listeners: Set<Listener> = new Set();
let currentMode: DiffViewMode = loadDiffViewMode();

function subscribe(listener: Listener): () => void {
  listeners.add(listener);
  return () => listeners.delete(listener);
}

function getSnapshot(): DiffViewMode {
  return currentMode;
}

function getServerSnapshot(): DiffViewMode {
  // SSR fallback — use the default since we don't have localStorage on
  // the server. Tests that need a specific value should mock via
  // `localStorage.setItem` before render.
  return 'unified';
}

/** Single hook for the Unified/Split mode. The store is the only
 *  source of truth — `useSyncExternalStore` ensures every subscriber
 *  re-renders when the value flips, and the shell/card/stacked-review
 *  consumers all converge on the same instance. Issue #1374. */
export function useDiffViewMode(): [DiffViewMode, (m: DiffViewMode) => void] {
  const mode = useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot);
  const setMode = (m: DiffViewMode) => {
    if (m === currentMode) return;
    currentMode = m;
    persistDiffViewMode(m);
    // Copy before iterate — a listener may unsubscribe during dispatch.
    for (const l of [...listeners]) l();
  };
  return [mode, setMode];
}

/** Test seam — reset the module-level store between tests so the
 *  `useSyncExternalStore` subscribers in one test don't leak the
 *  `split` preference into the next test (and back). */
export function __resetDiffViewModeForTests(): void {
  currentMode = loadDiffViewMode();
  listeners = new Set();
}

/**
 * Shared diff renderer: a GitHub-style stacked, unified, syntax-highlighted
 * view. One `<FileDiffCard>` per changed file, each collapsible, with a hunk
 * header (`@@ … @@`) between change regions and per-line highlighting fed by
 * the backend's `lines_highlighted`. Used by the desktop review panel and
 * the PR diff overlay (`PrDiffView` reuses `<DiffLineList>` + `<HunkBlock>`
 * to render GitHub's raw `patch` text through the same body — issue #725).
 *
 * The `w-10` gutters and `/15` accent-fill opacity are the canonical visual
 * contract — every other diff surface matches them.
 */

/** Canonical `+`/`-` background opacity. Issue #725 picked this to match the
 *  header summary tokens (vs `/10` which DiffView used before being deleted). */
export const DIFF_LINE_BG = {
  add: 'bg-accent-green/15',
  remove: 'bg-accent-red/15',
} as const;

/** Canonical gutter width — w-10 for line numbers + w-4 for the marker. */
export const DIFF_GUTTER_WIDTH = 'w-10';

/**
 * Re-export under the historical `statusMeta` name so callers that imported
 * it from `'../Diff/Diff'` (FileTree.tsx) keep working without a churn
 * rename pass. The canonical home is `lib/status.ts` — new callers should
 * import `fileDiffStatusMeta` from there directly.
 */
export { fileDiffStatusMeta as statusMeta } from '../../lib/status';

/** A stable DOM id so a file-list can scroll a card into view. */
export function diffCardId(path: string): string {
  return `diff-file-${path}`;
}

function lineBg(type: DiffLine['line_type']): string {
  if (type === 'add') return DIFF_LINE_BG.add;
  if (type === 'remove') return DIFF_LINE_BG.remove;
  return '';
}

function marker(type: DiffLine['line_type']): string {
  return type === 'add' ? '+' : type === 'remove' ? '-' : ' ';
}

function markerColor(type: DiffLine['line_type']): string {
  if (type === 'add') return 'text-accent-green';
  if (type === 'remove') return 'text-accent-red';
  return 'text-text-muted/40';
}

/** One row of a diff: line-number gutters, +/- marker, content (optionally
 *  pre-highlighted HTML). Exported so `PrDiffView` renders PR raw patches
 *  through the same body and inherits the canonical gutter + bg opacity. */
export function DiffLineRow({ line, html }: { line: DiffLine; html?: string }) {
  return (
    <div className={`flex ${lineBg(line.line_type)}`}>
      <span className={`${DIFF_GUTTER_WIDTH} px-1 text-right text-text-muted/50 select-none flex-shrink-0`}>
        {line.old_num ?? ''}
      </span>
      <span className={`${DIFF_GUTTER_WIDTH} px-1 text-right text-text-muted/50 select-none flex-shrink-0`}>
        {line.new_num ?? ''}
      </span>
      <span
        className={`w-4 text-center select-none flex-shrink-0 ${markerColor(line.line_type)}`}
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

/** A hunk: `@@ … @@` header + the lines it covers. Exported so `PrDiffView`
 *  can package a section of its raw patch text into a `HunkBlock` and render
 *  it through the same body — `PrPatch` builds `DiffLine[]` per `@@` group
 *  and threads them through here. */
export function HunkBlock({ hunk, last }: { hunk: DiffHunk; last: boolean }) {
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

/** One cell of a split row: gutter (its own side's line number) + content.
 *  `html` is the preserved syntect server-side token HTML (issue #1374). */
function SplitCell({
  line,
  html,
  side,
}: {
  line: DiffLine | null;
  html?: string;
  side: 'old' | 'new';
}) {
  const type = line?.line_type;
  const bg =
    type === 'add' && side === 'new'
      ? DIFF_LINE_BG.add
      : type === 'remove' && side === 'old'
        ? DIFF_LINE_BG.remove
        : '';
  return (
    <div className={`flex flex-1 min-w-0 ${bg}`}>
      <span className={`${DIFF_GUTTER_WIDTH} px-1 text-right text-text-muted/50 select-none flex-shrink-0`}>
        {/* Left pane shows the old file's line numbers, right pane the
            new file's — matching GitHub's split view (#1374). */}
        {side === 'old' ? line?.old_num ?? '' : line?.new_num ?? ''}
      </span>
      <span className={`w-4 text-center select-none flex-shrink-0 ${markerColor(type ?? 'context')}`}>
        {line ? marker(line.line_type) : ''}
      </span>
      {line && html ? (
        // Highlighted HTML is generated server-side by syntect (trusted,
        // not user-controlled markup), one span run per line.
        <span
          className="whitespace-pre flex-1 min-w-0 overflow-x-auto"
          dangerouslySetInnerHTML={{ __html: html }}
        />
      ) : (
        <span className="whitespace-pre flex-1 min-w-0">{line?.content ?? ''}</span>
      )}
    </div>
  );
}

/** One row of a side-by-side diff: left (old) cell + right (new) cell. */
function SplitRowView({ row }: { row: SplitRow }) {
  return (
    <div className="flex">
      <SplitCell line={row.old} html={row.oldHtml} side="old" />
      <div className="w-px bg-border-subtle flex-shrink-0" />
      <SplitCell line={row.new} html={row.newHtml} side="new" />
    </div>
  );
}

/** A hunk rendered side-by-side: `@@ … @@` header + aligned rows. Rows are
 *  paired by `alignHunkRows` (issue #1374) — a remove+add run shares one
 *  row, so left and right panes stay in sync without line desynchronization. */
export function SplitHunkBlock({ hunk, last }: { hunk: DiffHunk; last: boolean }) {
  return (
    <div className={last ? '' : 'border-b border-border-subtle'}>
      <div className="px-2 py-0.5 bg-bg-overlay/60 text-text-muted select-none">
        @@ -{hunk.old_start},{hunk.old_lines} +{hunk.new_start},{hunk.new_lines} @@
      </div>
      {alignHunkRows(hunk).map((row, i) => (
        <SplitRowView key={i} row={row} />
      ))}
    </div>
  );
}

export function FileDiffCard({
  file,
  defaultOpen = true,
  onOpenFile,
  mode,
  onModeChange,
}: {
  file: FileDiff;
  defaultOpen?: boolean;
  /** When set, the header shows an "open in the center overlay" button that
   *  calls this with the file's path (issue #379). Omit it where there's no
   *  spacious surface to open into (e.g. the overlay renders its own diff). */
  onOpenFile?: (path: string) => void;
  /** Diff view mode — REQUIRED (issue #1374). Pass the value from
   *  `useDiffViewMode()`; the inline per-card toggle calls
   *  `onModeChange` to propagate. */
  mode: DiffViewMode;
  /** Called when the inline per-card toggle flips the mode — the
   *  parent decides whether to persist (typically via
   *  `useDiffViewMode()`'s setter, which already writes
   *  localStorage). */
  onModeChange: (mode: DiffViewMode) => void;
}) {
  const [open, setOpen] = useState(defaultOpen);
  // `mode` is fully controlled by the parent — no local state, no
  // localStorage write here. The inline toggle calls `onModeChange`,
  // which propagates to whatever store the parent uses
  // (`useDiffViewMode()` is the canonical sink).
  const meta = fileDiffStatusMeta(file.status);
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
        <span className="text-text-muted w-3 text-center text-2xs flex-shrink-0">
          {open ? '▼' : '▶'}
        </span>
        <span
          className={`font-bold w-3 flex-shrink-0 ${meta.color}`}
          title={meta.label}
        >
          {meta.letter}
        </span>
        <span className="flex-1 truncate font-mono text-xs min-w-0">
          {file.old_path && file.old_path !== file.path && (
            <span className="text-text-secondary">
              {file.old_path} <span aria-hidden="true">→</span>{' '}
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
        {/* Unified/Split toggle (issue #1374) — a sibling of the open-in-
            overlay button, same invalid-HTML-in-button reasoning. Mode is
            fully controlled; the click calls `onModeChange` which
            propagates to the shell/store. */}
        <span
          role="button"
          tabIndex={0}
          aria-label={`Toggle ${mode === 'split' ? 'unified' : 'split'} diff view`}
          title={mode === 'split' ? 'Switch to unified view' : 'Switch to split view'}
          onClick={(e) => {
            e.stopPropagation();
            onModeChange(mode === 'split' ? 'unified' : 'split');
          }}
          onKeyDown={(e) => {
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault();
              e.stopPropagation();
              onModeChange(mode === 'split' ? 'unified' : 'split');
            }
          }}
          className="flex-shrink-0 text-text-muted hover:text-accent-cyan transition-colors cursor-pointer font-mono text-2xs px-1"
        >
          {mode === 'split' ? '⬛⬛' : '⬛'}
        </span>
        {onOpenFile && (
          // Rendered as a sibling — nesting a <button> inside the header
          // button is invalid HTML and breaks click handling. `stopPropagation`
          // keeps the expand from also toggling the card's collapse.
          //
          // On click: collapse THIS card before delegating to the parent.
          // The centre overlay is about to render the same diff at full
          // width, so leaving the inline card expanded would show two copies
          // at once (issue #758). Other cards stay expanded, so the probe
          // remains a navigable file list — preserves #379's "probe stays
          // open and interactive" contract uniformly across all probe tabs.
          <span
            role="button"
            tabIndex={0}
            aria-label={`Open ${file.path} in the center diff overlay`}
            title="Open in center workspace"
            onClick={(e) => {
              e.stopPropagation();
              setOpen(false);
              onOpenFile(file.path);
            }}
            onKeyDown={(e) => {
              if (e.key === 'Enter' || e.key === ' ') {
                e.preventDefault();
                e.stopPropagation();
                setOpen(false);
                onOpenFile(file.path);
              }
            }}
            className="flex-shrink-0 text-text-muted hover:text-accent-cyan transition-colors cursor-pointer"
          >
            <svg
              width="13"
              height="13"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
              aria-hidden="true"
            >
              <path d="M15 3h6v6M14 10l7-7M21 14v5a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h5" />
            </svg>
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
          ) : mode === 'split' ? (
            file.hunks.map((hunk, i) => (
              <SplitHunkBlock
                key={i}
                hunk={hunk}
                last={i === file.hunks.length - 1}
              />
            ))
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

/** Stacked review surface: every changed file's diff in one scroll column.
 *  Issue #1374: `mode` switches between the unified stacked format and a
 *  2-column side-by-side (split) format with synchronized scrolling — the
 *  panes scroll together because each split row is ONE flex row, so row
 *  heights match by construction and the gutters never desynchronize.
 *  The mode is fully controlled by the caller; pass values from
 *  `useDiffViewMode()` for the standalone stacked-review case or from
 *  `DiffOverlayShell`'s render-prop for the overlay case. */
export function Diff({
  files,
  onOpenFile,
  mode,
  onModeChange,
}: {
  files: FileDiff[];
  /** Threaded to each card's "open in center overlay" affordance (#379). */
  onOpenFile?: (path: string) => void;
  /** Diff view mode — REQUIRED (issue #1374). Pass the value from
   *  `useDiffViewMode()`. */
  mode: DiffViewMode;
  /** Called when any card's inline toggle flips the mode. */
  onModeChange: (mode: DiffViewMode) => void;
}) {
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
        <FileDiffCard
          key={file.path}
          file={file}
          onOpenFile={onOpenFile}
          mode={mode}
          onModeChange={onModeChange}
        />
      ))}
    </div>
  );
}