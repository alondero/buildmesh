import { useEffect, useRef } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { TERMINAL_OPTIONS } from './terminalConfig';

interface BuildRunTerminalProps {
  sessionId: number;
  mode?: 'build' | 'run';
  useWorktree?: boolean;
  onClose?: () => void;
}

interface BuildRunOutputPayload {
  data: string;
}

function decodeBase64Bytes(data: string): Uint8Array {
  const binary = globalThis.atob(data);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
}

export function BuildRunTerminal({ sessionId, mode = 'build', useWorktree = true, onClose }: BuildRunTerminalProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const unlistenRef = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    if (!containerRef.current) return;
    let cancelled = false;

    const term = new Terminal(TERMINAL_OPTIONS);
    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);

    term.open(containerRef.current);
    fitAddon.fit();

    termRef.current = term;
    fitAddonRef.current = fitAddon;

    const resizeObserver = new ResizeObserver(() => {
      requestAnimationFrame(() => {
        fitAddonRef.current?.fit();
      });
    });
    resizeObserver.observe(containerRef.current);

    const eventName = `build-run-output-${sessionId}`;
    listen<string | BuildRunOutputPayload>(eventName, (event) => {
      if (typeof event.payload === 'string') {
        term.write(event.payload);
      } else {
        term.write(decodeBase64Bytes(event.payload.data));
      }
    }).then(unlisten => {
      if (cancelled) {
        unlisten();
        return;
      }
      unlistenRef.current = unlisten;
      term.write(`${mode === 'build' ? 'Building' : 'Running'}${useWorktree ? ' from worktree' : ''}...\r\n`);
      invoke('build_run', { nodeId: sessionId, mode }).catch(err => {
        term.write(`\r\nError: ${String(err)}\r\n`);
      });
    });

    return () => {
      cancelled = true;
      resizeObserver.disconnect();
      unlistenRef.current?.();
      term.dispose();
      invoke('close_build_run', { nodeId: sessionId }).catch(() => {});
    };
  }, [sessionId, mode]);

  return (
    <div className="flex flex-col flex-1 overflow-hidden bg-bg-overlay border-t border-border-default">
      <div className="flex items-center justify-between px-2 py-1 bg-bg-base border-b border-border-default">
        <span className="text-[10px] font-mono text-text-muted">
          {mode === 'build' ? 'Build' : 'Run'}{useWorktree ? ': worktree' : ''}
        </span>
        <button
          onClick={onClose}
          className="w-4 h-4 flex items-center justify-center rounded text-text-muted hover:text-accent-cyan hover:bg-bg-overlay transition-colors text-[10px]"
          title="Close build/run terminal"
        >
          ×
        </button>
      </div>
      <div
        ref={containerRef}
        className="flex-1 overflow-hidden"
        style={{ padding: '4px' }}
      />
    </div>
  );
}
