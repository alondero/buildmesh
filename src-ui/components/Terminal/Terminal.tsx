import { useState, useEffect, useRef, useCallback } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';

interface TerminalPanelProps {
  workspaceId: number;
  isWsl: boolean;
}

export function TerminalPanel({ workspaceId, isWsl }: TerminalPanelProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const ptyIdRef = useRef(`term-${workspaceId}-${Date.now()}`);
  const [connected, setConnected] = useState(false);

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
    const ptyId = ptyIdRef.current;

    try {
      await invoke('spawn_shell', {
        ptyId,
        isWsl,
        cwd: '',
      });
      setConnected(true);

      // Handle resize
      const observer = new ResizeObserver(() => fitAddon.fit());
      observer.observe(containerRef.current);

      term.onData((data) => {
        invoke('write_pty', { ptyId, data });
      });

      // Listen for PTY output
      const unlisten = await listen<{ pty_id: string; data: string }>('pty-output', (event) => {
        if (event.payload.pty_id === ptyId) {
          term.write(event.payload.data);
        }
      });

      return () => {
        unlisten();
        observer.disconnect();
        invoke('close_pty', { ptyId });
        term.dispose();
      };
    } catch (e) {
      term.write(`Failed to start terminal: ${e}\r\n`);
    }
  }, [isWsl]);

  useEffect(() => {
    const cleanup = connect();
    return () => {
      cleanup.then(fn => fn && fn());
    };
  }, [connect]);

  return (
    <div className="flex flex-col h-full">
      <div className="flex items-center justify-between px-3 py-1.5 border-b border-[#2a2a2a] bg-[#111]">
        <h3 className="text-xs font-medium text-[#888] uppercase">Terminal</h3>
        <span className={`text-xs ${connected ? 'text-[#22c55e]' : 'text-[#666]'}`}>
          {connected ? '●' : '○'}
        </span>
      </div>
      <div ref={containerRef} className="flex-1 overflow-hidden" style={{ height: 0 }} />
    </div>
  );
}
