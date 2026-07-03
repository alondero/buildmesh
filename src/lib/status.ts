import type { FileDiffStatus } from './tauri';

export type SessionStatus =
  | 'pending'
  | 'spawning'
  | 'running'
  | 'idle'
  | 'awaiting_input'
  | 'error'
  | 'suspended';

export const STATUS_CONFIG = {
  // Stage-2 in progress; visually pulses so the user sees liveness.
  pending: {
    color: 'text-text-muted animate-pulse-fast',
    bgColor: 'bg-text-muted animate-pulse-fast',
    dot: '◌',
    label: 'Starting…',
  },
  // Issue #654 — agent launched but the 3s early-exit window hasn't elapsed.
  // Visually mirrors `pending`; conditional promotion to Running fires next.
  spawning: {
    color: 'text-text-muted animate-pulse-fast',
    bgColor: 'bg-text-muted animate-pulse-fast',
    dot: '◌',
    label: 'Starting…',
  },
  running: {
    color: 'status-running',
    bgColor: 'bg-accent-cyan',
    dot: '●',
    label: 'Running',
  },
  idle: {
    color: 'status-idle',
    bgColor: 'bg-accent-cyan',
    dot: '○',
    label: 'Idle',
  },
  awaiting_input: {
    color: 'status-waiting animate-pulse-fast',
    bgColor: 'bg-status-warning animate-pulse-fast',
    dot: '●',
    label: 'Needs attention',
  },
  error: {
    color: 'status-error',
    bgColor: 'bg-status-error',
    dot: '✗',
    label: 'Error',
  },
  suspended: {
    color: 'text-violet',
    bgColor: 'bg-accent-violet',
    dot: '⏸',
    label: 'Suspended',
  },
} as const;

export function getStatusConfig(status: string | undefined | null) {
  if (!status) return STATUS_CONFIG.idle;
  return STATUS_CONFIG[status as keyof typeof STATUS_CONFIG] || STATUS_CONFIG.idle;
}

// ---------------------------------------------------------------------------
// File-diff status meta (issue #725). The badge "A/M/D/R/? + coloured letter"
// shown on a file card / tree row was duplicated three ways before this
// consolidation: `<Diff>` owned the table, `ChangedFilesSection` held two
// parallel maps (statusColors / statusPrefix), and `PrDiffView` had its own
// minimal copy. The shared review surface, the file tree, and the PR list
// all read from this single source of truth.
// ---------------------------------------------------------------------------

export interface FileDiffStatusMeta {
  letter: string;
  label: string;
  /** Tailwind text colour token for the badge. */
  color: string;
}

// Mirrors `FileDiffStatus` in `lib/tauri.ts` (which is the hand-typed
// closed-set alias; the generated `FileDiff.status` is the wider `string`
// — see ADR-0009). Unknown statuses fall back to the `modified` row so a
// drifted vocabulary doesn't render blank badges.
const FILE_DIFF_STATUS_META: Record<FileDiffStatus, FileDiffStatusMeta> = {
  added: { letter: 'A', label: 'Added', color: 'text-accent-green' },
  modified: { letter: 'M', label: 'Modified', color: 'text-accent-amber' },
  deleted: { letter: 'D', label: 'Deleted', color: 'text-accent-red' },
  renamed: { letter: 'R', label: 'Renamed', color: 'text-accent-violet' },
  untracked: { letter: '?', label: 'Untracked', color: 'text-text-muted' },
};

export function fileDiffStatusMeta(status: string): FileDiffStatusMeta {
  return (
    FILE_DIFF_STATUS_META[status as FileDiffStatus] ?? FILE_DIFF_STATUS_META.modified
  );
}