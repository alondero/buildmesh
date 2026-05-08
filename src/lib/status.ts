export type SessionStatus = 'running' | 'idle' | 'awaiting_input' | 'error' | 'suspended';

export const STATUS_CONFIG = {
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
