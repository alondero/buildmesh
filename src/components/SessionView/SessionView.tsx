import { useState, useEffect } from 'react';
import { useSessionStore } from '../../stores/sessionStore';
import { AgentTerminal } from '../Terminal/Terminal';
import { CheckpointRail } from '../CheckpointRail/CheckpointRail';
import { FileTree } from '../FileTree/FileTree';
import { listen } from '@tauri-apps/api/event';
import { watchSession, unwatchSession } from '../../lib/tauri';

export function SessionView() {
  const { activeSession, checkpoints, killAgent, spawnAgent, createCheckpoint } = useSessionStore();
  const [fileTreeKey, setFileTreeKey] = useState(0);

  // Watch session for file changes
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
      <div className="flex-1 flex items-center justify-center text-[#666]">
        <div className="text-center">
          <p className="text-lg mb-2">No session selected</p>
          <p className="text-sm">Select a session from the sidebar to get started</p>
        </div>
      </div>
    );
  }

  const handleKillAgent = async () => {
    await killAgent(activeSession.id);
  };

  const handleSpawnAgent = async (provider: string) => {
    await spawnAgent(activeSession.id, provider);
  };

  const handleCheckpoint = async () => {
    if (!activeSession) return;
    const turnIndex = checkpoints.length + 1;
    await createCheckpoint(activeSession.id, turnIndex);
  };

  const envLabel = activeSession.env === 'wsl' ? 'WSL: Ubuntu' : 'Windows';
  const isRunning = activeSession.status === 'running';

  return (
    <div className="flex-1 flex flex-col h-full bg-[#0f0f0f]">
      {/* Header */}
      <div className="flex items-center gap-3 px-4 py-3 border-b border-[#2a2a2a]">
        <h2 className="text-sm font-semibold">{activeSession.name}</h2>
        <span className="text-xs px-2 py-0.5 rounded bg-[#1a1a1a] text-[#888] border border-[#2a2a2a]">
          {envLabel}
        </span>
        <span className="text-xs px-2 py-0.5 rounded bg-[#1a1a1a] text-[#888] border border-[#2a2a2a]">
          Branch: {activeSession.branch}
        </span>
        <span className="text-xs px-2 py-0.5 rounded bg-[#1a1a1a] text-[#888] border border-[#2a2a2a]">
          ⏸ {checkpoints.length}
        </span>
        <button
          onClick={handleCheckpoint}
          className="text-xs px-2 py-0.5 rounded bg-[#1a1a1a] text-[#888] border border-[#2a2a2a] hover:border-[#3b82f6] hover:text-[#3b82f6]"
          title="Create checkpoint"
        >
          + Checkpoint
        </button>

        <div className="ml-auto flex gap-2">
          {!isRunning ? (
            <>
              <button
                onClick={() => handleSpawnAgent('anthropic')}
                className="text-xs px-3 py-1 rounded bg-[#3b82f6] text-white hover:bg-[#2563eb]"
              >
                Anthropic
              </button>
              <button
                onClick={() => handleSpawnAgent('minimax')}
                className="text-xs px-3 py-1 rounded bg-[#6366f1] text-white hover:bg-[#4f46e5]"
              >
                Minimax
              </button>
              <button
                onClick={() => handleSpawnAgent('gemini')}
                className="text-xs px-3 py-1 rounded bg-[#10b981] text-white hover:bg-[#059669]"
              >
                Gemini
              </button>
              <button
                onClick={() => handleSpawnAgent('opencode')}
                className="text-xs px-3 py-1 rounded bg-[#f59e0b] text-white hover:bg-[#d97706]"
              >
                OpenCode
              </button>
            </>
          ) : (
            <button
              onClick={handleKillAgent}
              className="text-xs px-3 py-1 rounded bg-[#ef4444] text-white hover:bg-[#dc2626]"
            >
              Stop
            </button>
          )}
        </div>
      </div>

      {/* Checkpoint rail */}
      <CheckpointRail checkpoints={checkpoints} />

      {/* Main content area */}
      <div className="flex-1 flex overflow-hidden">
        {/* Agent terminal - takes full height, main area */}
        <div className="flex-1 min-w-0">
          <AgentTerminal sessionId={activeSession.id} />
        </div>

        {/* File tree sidebar */}
        <div className="w-72 border-l border-[#2a2a2a] overflow-y-auto">
          <div className="p-3">
            <h3 className="text-xs font-medium text-[#888] uppercase mb-2">Files</h3>
            <FileTree key={fileTreeKey} sessionPath={activeSession.path} />
          </div>
        </div>
      </div>
    </div>
  );
}