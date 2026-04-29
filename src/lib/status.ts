// Centralized session status constants
// These are shared between sessionStore.ts and Sidebar.tsx

export type SessionStatus = 'running' | 'idle' | 'awaiting_input' | 'error';

export const STATUS_CONFIG = {
  running: {
    color: 'text-[#22c55e]',
    dot: '●',
  },
  idle: {
    color: 'text-[#888]',
    dot: '○',
  },
  awaiting_input: {
    color: 'text-[#f59e0b]',
    dot: '◐',
  },
  error: {
    color: 'text-[#ef4444]',
    dot: '✗',
  },
} as const;

export function getStatusConfig(status: SessionStatus) {
  return STATUS_CONFIG[status];
}
