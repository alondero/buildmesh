import { useState, useEffect, useRef } from 'react';
import { useWorkspaceStore } from '../../stores/workspaceStore';
import { TerminalPanel } from '../Terminal/Terminal';
import { CheckpointRail } from '../CheckpointRail/CheckpointRail';
import { SlashCommandBar } from '../SlashCommands/SlashCommandBar';
import { listen } from '@tauri-apps/api/event';

interface ChatMessage {
  role: 'user' | 'assistant';
  content: string;
  tool_calls?: string;
}

export function SessionView() {
  const { activeWorkspace, checkpoints, killAgent, spawnAgent } = useWorkspaceStore();
  const [messages, setMessages] = useState<ChatMessage[]>([]);
  const [input, setInput] = useState('');
  const [streaming, setStreaming] = useState(false);
  const chatEndRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    chatEndRef.current?.scrollIntoView({ behavior: 'smooth' });
  }, [messages]);

  // Listen for agent output
  useEffect(() => {
    const unlisten = listen<{ workspace_id: number; line: string }>('agent-output', (event) => {
      if (activeWorkspace && event.payload.workspace_id === activeWorkspace.id) {
        setMessages(prev => [...prev, { role: 'assistant', content: event.payload.line }]);
        setStreaming(false);
      }
    });

    return () => { unlisten.then(fn => fn()); };
  }, [activeWorkspace]);

  if (!activeWorkspace) {
    return (
      <div className="flex-1 flex items-center justify-center text-[#666]">
        <div className="text-center">
          <p className="text-lg mb-2">No workspace selected</p>
          <p className="text-sm">Select a workspace from the sidebar to get started</p>
        </div>
      </div>
    );
  }

  const handleSend = async () => {
    if (!input.trim()) return;
    const userMsg = input;
    setInput('');
    setMessages(prev => [...prev, { role: 'user', content: userMsg }]);
    setStreaming(true);
    // Note: actual agent communication would go through the backend
    // For now we just display what was sent
  };

  const handleKillAgent = async () => {
    await killAgent(activeWorkspace.id);
    setMessages([]);
  };

  const handleSpawnAgent = async (provider: string) => {
    await spawnAgent(activeWorkspace.id, provider);
  };

  const envLabel = activeWorkspace.env === 'wsl' ? 'WSL: Ubuntu' : 'Windows';
  const isRunning = activeWorkspace.status === 'running';

  return (
    <div className="flex-1 flex flex-col h-full bg-[#0f0f0f]">
      {/* Header */}
      <div className="flex items-center gap-3 px-4 py-3 border-b border-[#2a2a2a]">
        <h2 className="text-sm font-semibold">{activeWorkspace.name}</h2>
        <span className="text-xs px-2 py-0.5 rounded bg-[#1a1a1a] text-[#888] border border-[#2a2a2a]">
          {envLabel}
        </span>
        <span className="text-xs px-2 py-0.5 rounded bg-[#1a1a1a] text-[#888] border border-[#2a2a2a]">
          Branch: {activeWorkspace.branch}
        </span>
        <span className="text-xs px-2 py-0.5 rounded bg-[#1a1a1a] text-[#888] border border-[#2a2a2a]">
          ⏸ {checkpoints.length}
        </span>

        <div className="ml-auto flex gap-2">
          {!isRunning ? (
            <>
              <button
                onClick={() => handleSpawnAgent('anthropic')}
                className="text-xs px-3 py-1 rounded bg-[#3b82f6] text-white hover:bg-[#2563eb]"
              >
                Run (Anthropic)
              </button>
              <button
                onClick={() => handleSpawnAgent('minimax')}
                className="text-xs px-3 py-1 rounded bg-[#6366f1] text-white hover:bg-[#4f46e5]"
              >
                Run (Minimax)
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
        {/* Chat panel */}
        <div className="flex-1 flex flex-col border-r border-[#2a2a2a]">
          <div className="flex-1 overflow-y-auto p-4 space-y-3">
            {messages.length === 0 && (
              <div className="text-center text-[#666] mt-20">
                <p className="text-sm">Agent not started yet</p>
                <p className="text-xs mt-1">Click "Run" above to spawn an agent</p>
              </div>
            )}

            {messages.map((msg, i) => (
              <div key={i} className={`flex ${msg.role === 'user' ? 'justify-end' : 'justify-start'}`}>
                <div
                  className={`
                    max-w-[80%] rounded-lg px-3 py-2 text-sm
                    ${msg.role === 'user'
                      ? 'bg-[#3b82f6] text-white'
                      : 'bg-[#1a1a1a] text-[#e0e0e0] border border-[#2a2a2a]'}
                  `}
                >
                  {msg.content}
                </div>
              </div>
            ))}

            {streaming && (
              <div className="flex justify-start">
                <div className="bg-[#1a1a1a] text-[#888] rounded-lg px-3 py-2 text-sm border border-[#2a2a2a]">
                  <span className="animate-pulse">●</span> Agent is thinking...
                </div>
              </div>
            )}

            <div ref={chatEndRef} />
          </div>

          {/* Slash commands */}
          <SlashCommandBar onCommand={(cmd) => setInput(cmd)} />

          {/* Input */}
          <div className="p-3 border-t border-[#2a2a2a]">
            <div className="flex gap-2">
              <input
                type="text"
                value={input}
                onChange={(e) => setInput(e.target.value)}
                onKeyDown={(e) => e.key === 'Enter' && handleSend()}
                placeholder="Send a message to the agent..."
                className="flex-1 bg-[#1a1a1a] border border-[#2a2a2a] rounded px-3 py-2 text-sm text-[#e0e0e0] placeholder-[#666] focus:border-[#3b82f6] focus:outline-none"
              />
              <button
                onClick={handleSend}
                className="px-4 py-2 rounded bg-[#3b82f6] text-white text-sm hover:bg-[#2563eb]"
              >
                Send
              </button>
            </div>
          </div>
        </div>

        {/* Right panel: file tree + terminal */}
        <div className="w-80 flex flex-col">
          {/* File tree placeholder */}
          <div className="h-1/2 border-b border-[#2a2a2a] p-3 overflow-y-auto">
            <h3 className="text-xs font-medium text-[#888] uppercase mb-2">Files</h3>
            <div className="text-xs text-[#666]">
              {activeWorkspace.status === 'running' ? (
                <p className="animate-pulse">Watching for changes...</p>
              ) : (
                <p>Agent not running</p>
              )}
            </div>
          </div>

          {/* Terminal */}
          <div className="h-1/2">
            <TerminalPanel workspaceId={activeWorkspace.id} isWsl={activeWorkspace.env === 'wsl'} />
          </div>
        </div>
      </div>
    </div>
  );
}
