import { create } from 'zustand';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { disposeTerminal } from '../components/Terminal/Terminal';

export interface Session {
  id: number;
  project_id: number;
  name: string;
  path: string;
  branch: string;
  env: 'windows' | 'wsl';
  provider: 'anthropic' | 'minimax' | 'gemini' | 'opencode';
  status: 'running' | 'idle' | 'awaiting_input' | 'error' | 'archived';
  cli_session_id?: string;
  created_at: string;
}

export interface Checkpoint {
  id: number;
  session_id: number;
  git_ref: string;
  turn_index: number;
  message: string;
  created_at: string;
}

interface SessionState {
  sessions: Session[];
  activeSessionId: number | null;
  gridSessionIds: number[];
  layout: 'single' | 'grid';
  checkpoints: Checkpoint[];
  loading: boolean;
  error: string | null;

  // Derived getter
  getActiveSession: () => Session | null;

  fetchSessions: () => Promise<void>;
  createSession: (projectId: number, name: string, path: string, branch: string) => Promise<void>;
  archiveSession: (id: number) => Promise<void>;
  restoreSession: (id: number) => Promise<void>;
  setActiveSession: (id: number | null) => Promise<void>;
  toggleGridSession: (id: number) => void;
  setLayout: (layout: 'single' | 'grid') => void;
  fetchCheckpoints: (sessionId: number) => Promise<void>;
  spawnAgent: (sessionId: number, provider: string) => Promise<void>;
  killAgent: (sessionId: number) => Promise<void>;
  sendToAgent: (sessionId: number, input: string) => Promise<void>;
  writeToAgent: (sessionId: number, data: string) => Promise<void>;
  createCheckpoint: (sessionId: number, turnIndex: number, message?: string) => Promise<void>;
  revertCheckpoint: (checkpointId: number) => Promise<void>;
  initAttentionListeners: () => Promise<void>;
}

export const useSessionStore = create<SessionState>((set, get) => ({
  sessions: [],
  activeSessionId: null,
  gridSessionIds: [],
  layout: 'single',
  checkpoints: [],
  loading: false,
  error: null,

  getActiveSession: () => {
    const { sessions, activeSessionId } = get();
    if (activeSessionId === null) return null;
    return sessions.find(s => s.id === activeSessionId) || null;
  },

  fetchSessions: async () => {
    set({ loading: true, error: null });
    try {
      const sessions = await invoke<Session[]>('list_sessions');
      set({ sessions, loading: false });
    } catch (e) {
      set({ error: String(e), loading: false });
    }
  },

  toggleGridSession: (id) => {
    set((state) => {
      const isSelected = state.gridSessionIds.includes(id);
      const newGridSessionIds = isSelected
        ? state.gridSessionIds.filter((gid) => gid !== id)
        : [...state.gridSessionIds, id].slice(-4);
      return { gridSessionIds: newGridSessionIds };
    });
  },

  setLayout: (layout) => set({ layout }),

  ...(() => {
    let listenersAttached = false;
    return {
      initAttentionListeners: async () => {
        if (listenersAttached) return;
        listenersAttached = true;

        await listen<{ session_id: number }>('attention-needed', (event) => {
          const sessionId = event.payload.session_id;
          set((state) => ({
            sessions: state.sessions.map((s) =>
              s.id === sessionId ? { ...s, status: 'awaiting_input' as const } : s
            ),
          }));
        });

        await listen<{ session_id: number }>('attention-cleared', (event) => {
          const sessionId = event.payload.session_id;
          set((state) => ({
            sessions: state.sessions.map((s) =>
              s.id === sessionId ? { ...s, status: 'running' as const } : s
            ),
          }));
        });
      },
    };
  })(),

  createSession: async (projectId, name, path, branch) => {
    try {
      const session = await invoke<Session>('create_session', {
        projectId, name, path, branch,
      });
      set((state) => ({ sessions: [session, ...state.sessions] }));
      await get().setActiveSession(session.id);
    } catch (e) {
      set({ error: String(e) });
    }
  },

  archiveSession: async (id) => {
    try {
      await invoke('archive_session', { sessionId: id });
      disposeTerminal(id); 
      await get().fetchSessions();
      if (get().activeSessionId === id) {
        set({ activeSessionId: null });
      }
    } catch (e) {
      set({ error: String(e) });
    }
  },

  restoreSession: async (id) => {
    try {
      await invoke('restore_session', { sessionId: id });
      await get().fetchSessions();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  setActiveSession: async (id) => {
    set({ activeSessionId: id });
    if (id !== null) {
      await get().fetchCheckpoints(id);
    } else {
      set({ checkpoints: [] });
    }
  },

  fetchCheckpoints: async (sessionId) => {
    try {
      const checkpoints = await invoke<Checkpoint[]>('list_checkpoints', { sessionId });
      set({ checkpoints });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  spawnAgent: async (sessionId, provider) => {
    try {
      const session = get().sessions.find(s => s.id === sessionId);
      await invoke('spawn_agent', { 
        sessionId, 
        provider,
        resume: session?.cli_session_id 
      });
      await get().fetchSessions();
    } catch (e) {
      console.error('[sessionStore] spawnAgent failed:', e);
      set({ error: String(e) });
      // Re-throw to allow component-level handling if needed
      throw e;
    }
  },

  killAgent: async (sessionId) => {
    try {
      await invoke('kill_agent', { sessionId });
      await get().fetchSessions();
    } catch (e) {
      set({ error: String(e) });
    }
  },

  sendToAgent: async (sessionId, input) => {
    try {
      await invoke('send_to_agent', { sessionId, input });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  writeToAgent: async (sessionId, data) => {
    try {
      await invoke('write_to_agent', { sessionId, data });
    } catch (e) {
      set({ error: String(e) });
    }
  },

  createCheckpoint: async (sessionId, turnIndex, message) => {
    try {
      await invoke('create_checkpoint', { sessionId, turnIndex, message });
      await get().fetchCheckpoints(sessionId);
    } catch (e) {
      set({ error: String(e) });
    }
  },

  revertCheckpoint: async (checkpointId) => {
    try {
      await invoke('revert_to_checkpoint', { checkpointId });
      const { activeSessionId } = get();
      if (activeSessionId) await get().fetchCheckpoints(activeSessionId);
    } catch (e) {
      set({ error: String(e) });
    }
  },
}));
