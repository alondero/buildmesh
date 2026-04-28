import { useState, useEffect, useMemo } from 'react';
import { useSessionStore, Session } from '../../stores/sessionStore';
import { AgentTerminal } from '../Terminal/Terminal';
import { CheckpointRail } from '../CheckpointRail/CheckpointRail';
import { FileTree } from '../FileTree/FileTree';
import { SlashCommandBar } from '../SlashCommands/SlashCommandBar';
import { listen } from '@tauri-apps/api/event';
import { watchSession, unwatchSession } from '../../lib/tauri';
import { STATUS_CONFIG } from '../../lib/status';

export function SessionView() {
  const { 
    sessions,
    checkpoints, 
    layout,
    gridSessionIds,
    getActiveSession,
    killAgent, 
    spawnAgent, 
    createCheckpoint,
    setActiveSession,
    sendToAgent,
    setLayout,
    toggleGridSession
  } = useSessionStore();
  
  const [fileTreeKey, setFileTreeKey] = useState(0);
  const activeSession = getActiveSession();

  const tabSessions = useMemo(() => {
    return sessions.filter(s => s.status !== 'archived').slice(0, 10);
  }, [sessions]);

  useEffect(() => {
    if (!activeSession) return;
    watchSession(activeSession.id).catch(console.error);
    const unlisten = listen<{ session_id: number; change: { path: string; kind: string } }>('file-change', (event) => {
      if (activeSession && event.payload.session_id === activeSession.id) {
        setFileTreeKey(k => k + 1);
      }
    });
    return () => {
      unwatchSession(activeSession.id).catch(console.error);
      unlisten.then(fn => fn());
    };
  }, [activeSession?.id]);

  if (!activeSession) {
    return (
      <div className="flex-1 flex flex-col bg-[#0f0f0f]">
        <div className="flex-1 flex items-center justify-center text-[#666]">
          <div className="text-center">
            <p className="text-lg mb-2 text-[#aaa]">Buildmesh Orchestrator</p>
            <p className="text-sm">Select a session to start managing your agents</p>
          </div>
        </div>
      </div>
    );
  }

  const handleSlashCommand = (cmd: string) => {
    if (!activeSession) return;
    sendToAgent(activeSession.id, cmd);
  };
  
  return (
    <div className="flex-1 flex flex-col h-full bg-[#0f0f0f] overflow-hidden">
      {/* Session Tabs Bar */}
      <div className="flex items-center bg-[#0a0a0a] border-b border-[#2a2a2a] px-2 h-10 overflow-x-auto no-scrollbar">
        {tabSessions.map(session => {
          const isActive = session.id === activeSession.id;
          const isInGrid = gridSessionIds.includes(session.id);
          const status = STATUS_CONFIG[session.status];
          return (
            <div key={session.id} className="flex items-center mr-0.5">
              <button
                data-session-tab={session.id}
                onClick={() => setActiveSession(session.id)}
                className={`
                  flex items-center gap-2 px-3 h-8 rounded-t-md text-xs transition-colors whitespace-nowrap
                  ${isActive
                    ? 'bg-[#1a1a1a] text-[#fff] border-x border-t border-[#2a2a2a]'
                    : 'text-[#666] hover:bg-[#151515] hover:text-[#aaa]'
                  }
                `}
              >
                <span className={`${status.color} text-[8px]`}>{status.dot}</span>
                <span className="max-w-[120px] truncate">{session.name}</span>
                {session.status === 'awaiting_input' && (
                  <span className="w-1.5 h-1.5 rounded-full bg-orange-500 animate-pulse" />
                )}
              </button>
              <button 
                onClick={(e) => { e.stopPropagation(); toggleGridSession(session.id); }}
                className={`h-8 px-1.5 rounded-t-md border-t border-r border-[#2a2a2a] text-[10px] transition-colors
                  ${isInGrid ? 'bg-blue-600/20 text-blue-400' : 'bg-[#0a0a0a] text-[#444] hover:text-[#888]'}
                  ${isActive ? 'border-l-0' : 'border-l border-[#2a2a2a]'}
                `}
                title="Toggle Grid View"
              >
                {isInGrid ? '▣' : '□'}
              </button>
            </div>
          );
        })}

        <div className="ml-auto flex items-center gap-2 px-2">
           <button 
             onClick={() => setLayout(layout === 'single' ? 'grid' : 'single')}
             className={`px-2 py-0.5 rounded text-[10px] font-bold border transition-colors
               ${layout === 'grid' 
                 ? 'bg-blue-600 border-blue-500 text-white' 
                 : 'bg-[#1a1a1a] border-[#2a2a2a] text-[#666] hover:text-[#aaa]'
               }
             `}
           >
             {layout === 'grid' ? 'GRID VIEW' : 'SINGLE VIEW'}
           </button>
        </div>
      </div>

      {/* Main content area */}
      <div className="flex-1 flex overflow-hidden">
        {layout === 'single' ? (
           <SingleLayout 
             activeSession={activeSession} 
             checkpoints={checkpoints}
             fileTreeKey={fileTreeKey}
             onSlashCommand={handleSlashCommand}
             onSpawnAgent={spawnAgent}
             onKillAgent={killAgent}
             onCreateCheckpoint={createCheckpoint}
           />
        ) : (
           <GridLayout 
             gridSessionIds={gridSessionIds}
             sessions={sessions}
             onSlashCommand={sendToAgent}
           />
        )}
      </div>
    </div>
  );
}

function SingleLayout({ 
  activeSession, 
  checkpoints, 
  fileTreeKey, 
  onSlashCommand,
  onSpawnAgent,
  onKillAgent,
  onCreateCheckpoint
}: any) {
  const isRunning = activeSession.status === 'running' || activeSession.status === 'awaiting_input';
  const envLabel = activeSession.env === 'wsl' ? 'WSL' : 'WIN';

  return (
    <div className="flex-1 flex overflow-hidden">
      <div className="flex-1 flex flex-col min-w-0">
        {/* Toolbar */}
        <div className="flex items-center gap-3 px-4 py-2 border-b border-[#2a2a2a] bg-[#0f0f0f]">
          <div className="flex flex-col">
            <div className="flex items-center gap-2">
              <h2 className="text-sm font-semibold">{activeSession.name}</h2>
              <span className="text-[10px] px-1.5 py-0.5 rounded bg-[#1a1a1a] text-[#666] border border-[#2a2a2a] font-mono">
                {envLabel}
              </span>
            </div>
            <span className="text-[10px] text-[#555] truncate max-w-[300px]">
              {activeSession.path}
            </span>
          </div>

          <div className="ml-auto flex gap-2">
            {!isRunning ? (
              <div className="flex items-center gap-1 bg-[#1a1a1a] p-1 rounded border border-[#2a2a2a]">
                <button onClick={() => onSpawnAgent(activeSession.id, 'anthropic')} className="text-[10px] px-2 py-1 rounded bg-[#3b82f6] text-white hover:bg-[#2563eb]">Anthropic</button>
                <button onClick={() => onSpawnAgent(activeSession.id, 'minimax')} className="text-[10px] px-2 py-1 rounded bg-[#6366f1] text-white hover:bg-[#4f46e5]">Minimax</button>
                <button onClick={() => onSpawnAgent(activeSession.id, 'gemini')} className="text-[10px] px-2 py-1 rounded bg-[#10b981] text-white hover:bg-[#059669]">Gemini</button>
                <button onClick={() => onSpawnAgent(activeSession.id, 'opencode')} className="text-[10px] px-2 py-1 rounded bg-[#f59e0b] text-white hover:bg-[#d97706]">OpenCode</button>
              </div>
            ) : (
              <button onClick={() => onKillAgent(activeSession.id)} className="text-xs px-3 py-1 rounded bg-[#ef4444]/20 text-[#ef4444] border border-[#ef4444]/30 hover:bg-[#ef4444]/30">Stop Agent</button>
            )}
            <div className="h-6 w-px bg-[#2a2a2a] mx-1" />
            <button
              onClick={() => onCreateCheckpoint(activeSession.id, checkpoints.length + 1)}
              className="text-xs px-2 py-1 rounded bg-[#1a1a1a] text-[#888] border border-[#2a2a2a] hover:border-[#3b82f6] hover:text-[#3b82f6]"
            >
              + Checkpoint ({checkpoints.length})
            </button>
          </div>
        </div>

        <div className="flex-1 overflow-hidden">
          <AgentTerminal sessionId={activeSession.id} />
        </div>
        <SlashCommandBar onCommand={onSlashCommand} />
      </div>

      <div className="w-64 border-l border-[#2a2a2a] flex flex-col bg-[#0a0a0a]">
        <div className="p-3 border-b border-[#2a2a2a] bg-[#111]">
          <h3 className="text-xs font-medium text-[#888] uppercase">Files</h3>
        </div>
        <div className="flex-1 overflow-y-auto p-2">
          <FileTree key={fileTreeKey} sessionPath={activeSession.path} />
        </div>
        <CheckpointRail checkpoints={checkpoints} />
      </div>
    </div>
  );
}

function GridLayout({ gridSessionIds, sessions, onSlashCommand }: any) {
  if (gridSessionIds.length === 0) {
    return (
      <div className="flex-1 flex items-center justify-center text-[#444]">
        <p>No sessions selected for Grid View. Click the □ icon on session tabs.</p>
      </div>
    );
  }

  return (
    <div className={`flex-1 grid gap-1 p-1 bg-[#1a1a1a] 
      ${gridSessionIds.length <= 1 ? 'grid-cols-1' : gridSessionIds.length <= 2 ? 'grid-cols-2' : 'grid-cols-2 grid-rows-2'}
    `}>
      {gridSessionIds.map((id: number) => {
        const session = sessions.find((s: Session) => s.id === id);
        return (
          <div key={id} className="flex flex-col bg-[#0f0f0f] border border-[#2a2a2a] rounded overflow-hidden">
            <div className="flex items-center justify-between px-2 py-1 bg-[#111] border-b border-[#2a2a2a]">
              <span className="text-[10px] font-bold text-[#aaa] truncate">{session?.name}</span>
              <span className="text-[9px] text-[#444]">{session?.env.toUpperCase()}</span>
            </div>
            <div className="flex-1 overflow-hidden">
              <AgentTerminal sessionId={id} />
            </div>
            <SlashCommandBar onCommand={(cmd) => onSlashCommand(id, cmd)} />
          </div>
        );
      })}
    </div>
  );
}
