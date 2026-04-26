import { useEffect, useRef } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { listen } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';

// Module-level registry - persists across session switches
// Each session gets its own xterm.js instance with its own scrollback buffer
const TERMINALS = new Map<number, Terminal>();
const FIT_ADDONS = new Map<number, FitAddon>();
const LISTENERS = new Map<number, () => void>();

interface AgentTerminalProps {
  sessionId: number;
}

async function writeToAgent(sessionId: number, data: string) {
  try {
    await invoke('write_to_agent', { sessionId, data });
  } catch (e) {
    console.error('write_to_agent error:', e);
  }
}

function getOrCreateTerminal(sessionId: number): Terminal {
  if (TERMINALS.has(sessionId)) {
    return TERMINALS.get(sessionId)!;
  }

  const term = new Terminal({
    theme: { background: '#0f0f0f', foreground: '#e0e0e0' },
    fontSize: 13,
    fontFamily: 'Cascadia Code, Consolas, monospace',
    scrollback: 10000,
  });
  const fitAddon = new FitAddon();
  term.loadAddon(fitAddon);

  // Set up event listener for this session
  listen<{ session_id: number; line: string }>('agent-output', (event) => {
    if (event.payload.session_id === sessionId) {
      term.write(event.payload.line);
    }
  }).then((unlisten) => {
    LISTENERS.set(sessionId, () => unlisten());
  });

  // Input forwarding
  term.onData((data) => {
    writeToAgent(sessionId, data);
  });

  TERMINALS.set(sessionId, term);
  FIT_ADDONS.set(sessionId, fitAddon);
  return term;
}

export function disposeTerminal(sessionId: number) {
  const unlisten = LISTENERS.get(sessionId);
  if (unlisten) {
    unlisten();
    LISTENERS.delete(sessionId);
  }
  const term = TERMINALS.get(sessionId);
  if (term) {
    term.dispose();
    TERMINALS.delete(sessionId);
  }
  FIT_ADDONS.delete(sessionId);
}

export function AgentTerminal({ sessionId }: AgentTerminalProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const prevSessionIdRef = useRef<number | null>(null);

  useEffect(() => {
    if (!containerRef.current) {
      return;
    }

    // Clean up the previous session's terminal BEFORE mounting the new one.
    // This ensures the old Terminal's DOM is removed so xterm.js starts fresh.
    if (prevSessionIdRef.current !== null && prevSessionIdRef.current !== sessionId) {
      disposeTerminal(prevSessionIdRef.current);
    }

    prevSessionIdRef.current = sessionId;

    const terminal = getOrCreateTerminal(sessionId);
    try {
      terminal.open(containerRef.current);
    } catch (e) {
      console.error('[AgentTerminal] terminal.open() failed:', e);
    }
    const fitAddon = FIT_ADDONS.get(sessionId);
    if (fitAddon) {
      fitAddon.fit();
    }

    const observer = new ResizeObserver(() => {
      const fitAddon = FIT_ADDONS.get(sessionId);
      if (fitAddon) {
        fitAddon.fit();
      }
    });
    observer.observe(containerRef.current);

    return () => {
      observer.disconnect();
    };
  }, [sessionId]);

  return (
    <div className="flex flex-col h-full">
      <div className="flex items-center justify-between px-3 py-1.5 border-b border-[#2a2a2a] bg-[#111]">
        <h3 className="text-xs font-medium text-[#888] uppercase">Agent Terminal</h3>
        <span className="text-xs text-[#22c55e]">●</span>
      </div>
      <div ref={containerRef} className="flex-1 overflow-hidden" />
    </div>
  );
}
