import { useState, useEffect, useRef, useCallback } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { listen } from '@tauri-apps/api/event';
import { useSessionStore } from '../../stores/sessionStore';

interface AgentTerminalProps {
  sessionId: number;
}

export function AgentTerminal({ sessionId }: AgentTerminalProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const [connected, setConnected] = useState(false);
  const writeToAgent = useSessionStore((s) => s.writeToAgent);

  const connect = useCallback(async () => {
    if (!containerRef.current) return;

    const term = new Terminal({
      theme: { background: '#0f0f0f', foreground: '#e0e0e0' },
      fontSize: 13,
      fontFamily: 'Cascadia Code, Consolas, monospace',
    });
    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);

    term.open(containerRef.current);
    fitAddon.fit();

    termRef.current = term;

    // Listen for agent output
    const unlisten = await listen<{ session_id: number; line: string }>('agent-output', (event) => {
      if (event.payload.session_id === sessionId) {
        term.write(event.payload.line);
      }
    });

    setConnected(true);

    // Handle resize
    const observer = new ResizeObserver(() => fitAddon.fit());

    // Send keystrokes directly to agent PTY
    term.onData((data) => {
      writeToAgent(sessionId, data);
    });

    return () => {
      unlisten();
      observer.disconnect();
      term.dispose();
    };
  }, [sessionId, writeToAgent]);

  useEffect(() => {
    const cleanup = connect();
    return () => {
      cleanup.then(fn => fn && fn());
    };
  }, [connect]);

  return (
    <div className="flex flex-col h-full">
      <div className="flex items-center justify-between px-3 py-1.5 border-b border-[#2a2a2a] bg-[#111]">
        <h3 className="text-xs font-medium text-[#888] uppercase">Agent Terminal</h3>
        <span className={`text-xs ${connected ? 'text-[#22c55e]' : 'text-[#666]'}`}>
          {connected ? '●' : '○'}
        </span>
      </div>
      <div ref={containerRef} className="flex-1 overflow-hidden" style={{ height: 0 }} />
    </div>
  );
}
