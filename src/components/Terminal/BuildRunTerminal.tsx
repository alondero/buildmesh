import { useEffect, useRef } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { listen, UnlistenFn } from '@tauri-apps/api/event';

interface BuildRunTerminalProps {
  sessionId: number;
  mode?: 'build' | 'run';
  onClose?: () => void;
}

export function BuildRunTerminal({ sessionId, mode = 'build', onClose }: BuildRunTerminalProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const unlistenRef = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    if (!containerRef.current) return;

    const term = new Terminal({
      theme: {
        background: '#09090f',
        foreground: '#e2e8f0',
        cursor: '#00d4ff',
        selectionBackground: 'rgba(0, 212, 255, 0.15)'
      },
      fontSize: 10,
      fontFamily: 'JetBrains Mono, Fira Code, Cascadia Code, Consolas, monospace',
      fontWeight: 500,
      scrollback: 10000,
      cursorBlink: true,
      allowProposedApi: true
    });

    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);

    term.open(containerRef.current);
    fitAddon.fit();

    termRef.current = term;
    fitAddonRef.current = fitAddon;

    // Set up resize observer
    const resizeObserver = new ResizeObserver(() => {
      fitAddonRef.current?.fit();
    });
    resizeObserver.observe(containerRef.current);

    // Listen for build/run output
    const eventName = `build-run-output-${sessionId}`;
    listen<{ line: string }>(eventName, (event) => {
      term.write(event.payload.line);
    }).then(unlisten => {
      unlistenRef.current = unlisten;
    });

    term.write(`${mode === 'build' ? 'Building' : 'Running'} from worktree...\r\n`);

    return () => {
      resizeObserver.disconnect();
      unlistenRef.current?.();
      term.dispose();
    };
  }, [sessionId, mode]);

  return (
    <div className="flex flex-col h-full bg-bg-overlay border-t border-border-default">
      {/* Terminal header */}
      <div className="flex items-center justify-between px-2 py-1 bg-bg-base border-b border-border-default">
        <span className="text-[10px] font-mono text-text-muted">
          {mode === 'build' ? 'Build' : 'Run'}: worktree
        </span>
        <button
          onClick={onClose}
          className="w-4 h-4 flex items-center justify-center rounded text-text-muted hover:text-accent-cyan hover:bg-bg-overlay transition-colors text-[10px]"
          title="Close build/run terminal"
        >
          ×
        </button>
      </div>
      {/* Terminal */}
      <div
        ref={containerRef}
        className="flex-1 overflow-hidden"
        style={{ padding: '4px' }}
      />
    </div>
  );
}
