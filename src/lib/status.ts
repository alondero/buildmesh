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
