import { useEffect, useMemo } from 'react';
import { useSessionStore, Session } from '../../stores/sessionStore';
import { useProjectStore } from '../../stores/projectStore';
import { AgentTerminal } from '../Terminal/Terminal';
import { SlashCommandBar } from '../SlashCommands/SlashCommandBar';
import { listen, emit } from '@tauri-apps/api/event';
import { watchSession, unwatchSession } from '../../lib/tauri';
import { getStatusConfig } from '../../lib/status';
import { FILE_CHANGE, FIT_TERMINALS } from '../../lib/events';

export function SessionView() {
  const selectedProjectId = useProjectStore(state => state.selectedProjectId);
  const {
    sessions,
    getActiveSession,
    setActiveSession,
    sendToAgent,
  } = useSessionStore();

  const activeSession = getActiveSession();

  const filteredSessions = useMemo(() => {
    if (selectedProjectId === null) {
      return sessions;
    }
    return sessions.filter(s => s.project_id === selectedProjectId);
  }, [sessions, selectedProjectId]);

  const tabSessions = useMemo(() => {
    return filteredSessions.slice(0, 10);
  }, [filteredSessions]);

  useEffect(() => {
    if (!activeSession) return;
    watchSession(activeSession.id).catch(console.error);
    const unlisten = listen<{ session_id: number; change: { path: string; kind: string } }>(FILE_CHANGE, () => {
      // File tree refresh handled by parent if needed
    });
    return () => {
      unwatchSession(activeSession.id).catch(console.error);
      unlisten.then(fn => fn());
    };
  }, [activeSession?.id]);

  // Dispatch fit-terminals event when active session changes
  // Auto-select first session when switching to a project that doesn't include the active session
  useEffect(() => {
    if (filteredSessions.length > 0 && activeSession && !filteredSessions.find(s => s.id === activeSession.id)) {
      // Active session is not in filtered sessions - auto-select the first one
      setActiveSession(filteredSessions[0].id);
    }
  }, [selectedProjectId, filteredSessions, activeSession, setActiveSession]);

  // Dispatch fit-terminals event when active session changes
  useEffect(() => {
    if (activeSession) {
      console.log(`[DEBUG SessionView] Emitting FIT_TERMINALS for session ${activeSession.id}`);
      emit(FIT_TERMINALS, { sessionId: activeSession.id }).catch(console.error);
    }
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeSession?.id]);

  const handleSlashCommand = (sessionId: number, cmd: string) => {
    sendToAgent(sessionId, cmd);
  };

  if (filteredSessions.length === 0) {
    return (
      <div className="flex-1 flex flex-col bg-bg-base">
        <div className="flex-1 flex items-center justify-center text-text-muted">
          <div className="text-center">
            <p className="text-lg mb-2 text-text-secondary">Buildmesh Orchestrator</p>
            <p className="text-sm">Select a session to start managing your agents</p>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="flex-1 flex flex-col h-full bg-bg-base overflow-hidden">
      {/* Session Tabs Bar */}
      <div className="flex items-center bg-bg-surface border-b border-border-subtle px-2 h-10 overflow-x-auto no-scrollbar">
        {tabSessions.map(session => {
          const isActive = session.id === activeSession?.id;
          const status = getStatusConfig(session.status);
          return (
            <button
              key={session.id}
              data-session-tab={session.id}
              onClick={() => setActiveSession(session.id)}
              className={`
                flex items-center gap-2 px-3 h-8 rounded-t-md text-xs transition-colors whitespace-nowrap mr-0.5
                ${isActive
                  ? 'bg-bg-overlay text-text-primary border-x border-t border-border-default'
                  : 'text-text-muted hover:bg-bg-card hover:text-text-secondary'
                }
              `}
            >
              <span className={`${status.color} text-[8px]`}>{status.dot}</span>
              <span className="max-w-[120px] truncate">{session.name}</span>
              {session.status === 'awaiting_input' && (
                <span className="w-1.5 h-1.5 rounded-full bg-orange-500 animate-pulse" />
              )}
            </button>
          );
        })}
      </div>

      {/* Grid of filtered sessions */}
      <GridLayout sessions={filteredSessions} onSlashCommand={handleSlashCommand} />
    </div>
  );
}

function GridLayout({ sessions, onSlashCommand }: { sessions: Session[]; onSlashCommand: (sessionId: number, cmd: string) => void }) {
  const count = sessions.length;
  let gridCols = "grid-cols-1";
  let gridRows = "grid-rows-1";

  if (count === 2) {
    gridCols = "grid-cols-2";
  } else if (count === 3) {
    gridCols = "grid-cols-2";
    gridRows = "grid-rows-2";
  } else if (count === 4) {
    gridCols = "grid-cols-2";
    gridRows = "grid-rows-2";
  } else if (count > 4) {
    gridCols = "grid-cols-3";
    gridRows = "grid-rows-2";
  }

  return (
    <div className={`flex-1 grid gap-1.5 p-1.5 bg-bg-surface ${gridCols} ${gridRows}`}>
      {sessions.map((session) => {
        return (
          <div key={session.id} className="flex flex-col bg-bg-card border border-border-default rounded-sm overflow-hidden group hover:border-accent-cyan/50 transition-colors">
            <div className="flex items-center justify-between px-2.5 py-1.5 bg-bg-overlay border-b border-border-default">
              <div className="flex items-center gap-2 overflow-hidden">
                <span className={`w-1.5 h-1.5 rounded-full ${
                  session.status === 'running' ? 'bg-accent-cyan' :
                  session.status === 'awaiting_input' ? 'bg-status-warning animate-pulse' :
                  'bg-text-muted'
                }`} />
                <span className="text-[11px] font-bold text-text-secondary truncate">{session.name}</span>
                {session.status === 'awaiting_input' && (
                  <span className="text-[9px] text-status-warning font-bold ml-1">ATTN</span>
                )}
              </div>
              <span className="text-[9px] text-text-muted font-mono px-1 rounded bg-bg-base">{session.env.toUpperCase()}</span>
            </div>
            <div className="flex-1 overflow-hidden bg-black">
              <AgentTerminal sessionId={session.id} />
            </div>
            <div className="opacity-40 group-hover:opacity-100 transition-opacity">
              <SlashCommandBar onCommand={(cmd) => onSlashCommand(session.id, cmd)} />
            </div>
          </div>
        );
      })}
    </div>
  );
}
