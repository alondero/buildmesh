import { useEffect, useRef, useState } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';

interface TerminalInstance {
  term: Terminal;
  fitAddon: FitAddon;
  unlisten: UnlistenFn;
}

class TerminalManager {
  private instances = new Map<number, TerminalInstance>();
  private listeners = new Set<() => void>();

  async getOrCreate(sessionId: number): Promise<TerminalInstance | null> {
    try {
      if (this.instances.has(sessionId)) {
        return this.instances.get(sessionId)!;
      }

      console.log(`[TerminalManager] Creating terminal for session ${sessionId}`);
      const term = new Terminal({
        theme: { 
          background: '#0f0f0f', 
          foreground: '#e0e0e0',
          cursor: '#3b82f6',
          selectionBackground: 'rgba(59, 130, 246, 0.3)'
        },
        fontSize: 13,
        fontFamily: 'Cascadia Code, Consolas, monospace',
        scrollback: 10000,
        cursorBlink: true,
        allowProposedApi: true
      });

      const fitAddon = new FitAddon();
      term.loadAddon(fitAddon);

      const unlisten = await listen<{ session_id: number; line: string }>('agent-output', (event) => {
        if (event.payload.session_id === sessionId) {
          term.write(event.payload.line);
        }
      });

      term.onData((data) => {
        invoke('write_to_agent', { sessionId, data }).catch(console.error);
      });

      const instance = { term, fitAddon, unlisten };
      this.instances.set(sessionId, instance);
      this.notify();
      return instance;
    } catch (e) {
      console.error(`[TerminalManager] Failed to create terminal for ${sessionId}`, e);
      return null;
    }
  }

  subscribe(cb: () => void) {
    this.listeners.add(cb);
    return () => { this.listeners.delete(cb); };
  }

  private notify() {
    this.listeners.forEach(cb => cb());
  }

  dispose(sessionId: number) {
    const instance = this.instances.get(sessionId);
    if (instance) {
      instance.unlisten();
      instance.term.dispose();
      this.instances.delete(sessionId);
      this.notify();
    }
  }
}

export const terminalManager = new TerminalManager();

/**
 * Backward compatibility export
 */
export function disposeTerminal(sessionId: number) {
  terminalManager.dispose(sessionId);
}

function TerminalContainer({ sessionId, isVisible }: { sessionId: number; isVisible: boolean }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const instanceRef = useRef<TerminalInstance | null>(null);

  useEffect(() => {
    let isActive = true;
    terminalManager.getOrCreate(sessionId).then(inst => {
      if (isActive && inst && containerRef.current) {
        instanceRef.current = inst;
        inst.term.open(containerRef.current);
        inst.fitAddon.fit();
      }
    });
    return () => { isActive = false; };
  }, [sessionId]);

  useEffect(() => {
    if (isVisible && instanceRef.current) {
      const inst = instanceRef.current;
      setTimeout(() => {
        inst.fitAddon.fit();
        inst.term.focus();
      }, 50);
    }
  }, [isVisible]);

  return (
    <div 
      ref={containerRef} 
      className={`h-full w-full ${isVisible ? 'block' : 'hidden'}`}
      style={{ padding: '8px' }}
    />
  );
}

export function TerminalStack({ activeSessionId }: { activeSessionId: number | null }) {
  const [sessionIds, setSessionIds] = useState<number[]>([]);
  const trackedIds = useRef<Set<number>>(new Set());

  useEffect(() => {
    if (activeSessionId !== null && !trackedIds.current.has(activeSessionId)) {
      trackedIds.current.add(activeSessionId);
      setSessionIds(Array.from(trackedIds.current));
    }
  }, [activeSessionId]);

  useEffect(() => {
    const unsubscribe = terminalManager.subscribe(() => {
      // Logic for manager-driven updates if needed
    });
    return () => unsubscribe();
  }, []);

  return (
    <div className="relative h-full w-full bg-[#0f0f0f] overflow-hidden">
      {sessionIds.map(id => (
        <TerminalContainer 
          key={id} 
          sessionId={id} 
          isVisible={id === activeSessionId} 
        />
      ))}
      
      {activeSessionId === null && (
        <div className="h-full w-full flex items-center justify-center text-[#444] text-xs font-mono uppercase tracking-widest">
          No Active Session
        </div>
      )}
    </div>
  );
}

export function AgentTerminal({ sessionId }: { sessionId: number }) {
  return <TerminalStack activeSessionId={sessionId} />;
}
