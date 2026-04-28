import { vi } from 'vitest';
import type { Session, Checkpoint } from '../../src/stores/sessionStore';

export interface MockSessionStore {
  sessions: Session[];
  activeSessionId: number | null;
  checkpoints: Checkpoint[];
  loading: boolean;
  error: string | null;
  getActiveSession: ReturnType<typeof vi.fn>;
  fetchSessions: ReturnType<typeof vi.fn>;
  createSession: ReturnType<typeof vi.fn>;
  setActiveSession: ReturnType<typeof vi.fn>;
  archiveSession: ReturnType<typeof vi.fn>;
  disposeTerminal: ReturnType<typeof vi.fn>;
  sendToAgent: ReturnType<typeof vi.fn>;
}

export function createMockSessionStore(initial: Partial<MockSessionStore> = {}): MockSessionStore {
  const sessions: Session[] = initial.sessions ?? [];
  const activeSessionId = initial.activeSessionId ?? null;

  return {
    sessions,
    activeSessionId,
    checkpoints: initial.checkpoints ?? [],
    loading: initial.loading ?? false,
    error: initial.error ?? null,
    getActiveSession: vi.fn(() => {
      if (activeSessionId === null) return null;
      return sessions.find(s => s.id === activeSessionId) ?? null;
    }),
    fetchSessions: vi.fn().mockResolvedValue(undefined),
    createSession: vi.fn().mockResolvedValue({ id: Date.now(), name: 'test-session' }),
    setActiveSession: vi.fn().mockImplementation(async (id: number | null) => {
      // This needs to update activeSessionId - we use a workaround since vi.fn() doesn't close over state
    }),
    archiveSession: vi.fn().mockResolvedValue(undefined),
    disposeTerminal: vi.fn(),
    sendToAgent: vi.fn().mockResolvedValue(undefined),
    ...initial,
  };
}

export function createMockSession(overrides: Partial<Session> = {}): Session {
  return {
    id: 1,
    project_id: 1,
    name: 'Test Session',
    path: '/test/path',
    branch: 'main',
    env: 'local' as const,
    provider: 'anthropic' as const,
    status: 'running' as const,
    cli_session_id: 'cli-session-1',
    created_at: new Date().toISOString(),
    ...overrides,
  };
}
