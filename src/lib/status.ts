// Centralized session status constants
// These are shared between sessionStore.ts and Sidebar.tsx

export type SessionStatus = 'running' | 'idle' | 'awaiting_input' | 'error' | 'suspended' | 'archived';

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
  suspended: {
    color: 'text-[#a78bfa]',
    dot: '⏸',
  },
  archived: {
    color: 'text-[#444]',
    dot: '📁',
  },
} as const;

export function getStatusConfig(status: string | undefined | null) {
  if (!status) return STATUS_CONFIG.idle;
  return STATUS_CONFIG[status as keyof typeof STATUS_CONFIG] || STATUS_CONFIG.idle;
}
