// Centralized agent node status constants
// These are shared between agentNodeStore.ts and Sidebar.tsx

export type SessionStatus = 'running' | 'idle' | 'awaiting_input' | 'error' | 'suspended' | 'archived';

export const STATUS_CONFIG = {
  running: {
    color: 'status-running',
    dot: '●',
  },
  idle: {
    color: 'status-idle',
    dot: '○',
  },
  awaiting_input: {
    color: 'status-waiting',
    dot: '◐',
  },
  error: {
    color: 'status-error',
    dot: '✗',
  },
  suspended: {
    color: 'text-violet',
    dot: '⏸',
  },
  archived: {
    color: 'status-archived',
    dot: '○',
  },
} as const;

export function getStatusConfig(status: string | undefined | null) {
  if (!status) return STATUS_CONFIG.idle;
  return STATUS_CONFIG[status as keyof typeof STATUS_CONFIG] || STATUS_CONFIG.idle;
}
