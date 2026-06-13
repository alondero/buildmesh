export type SessionStatus =
  | 'pending'
  | 'running'
  | 'idle'
  | 'awaiting_input'
  | 'error'
  | 'suspended';

export const STATUS_CONFIG = {
  // Node row exists but the slow stage-2 of spawn (git fetch, worktree
  // create, PTY spawn) has not yet completed. The two-stage spawn flow
  // (create_issue_node → start_node_background) flips this to 'running'
  // on stage-2 success or 'error' on failure. Visually pulses so the
  // user sees something is happening; the dot character is a hollow
  // circle (◌) to distinguish from the filled idle circle (○).
  pending: {
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
