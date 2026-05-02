import { useEffect, useRef } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { useSessionStore } from '../../stores/sessionStore';

interface TerminalInstance {
  term: Terminal;
  fitAddon: FitAddon;
  unlisten: UnlistenFn;
  opened: boolean;
  writeBuffer: string;
  frameRequested: boolean;
}

class TerminalManager {
  private instances = new Map<number, TerminalInstance>();
  private listeners = new Set<() => void>();
  private pending = new Map<number, Promise<TerminalInstance | null>>();

  // Exposed for TerminalStack to trigger focus/fit on active session
  getInstance(sessionId: number): TerminalInstance | undefined {
    return this.instances.get(sessionId);
  }

  async getOrCreate(sessionId: number): Promise<TerminalInstance | null> {
    // Return existing instance immediately if available
    if (this.instances.has(sessionId)) {
      console.log(`[DEBUG TerminalManager] getOrCreate(${sessionId}) - returning existing instance`);
      return this.instances.get(sessionId)!;
    }
    // If creation is already in progress, wait for it
    if (this.pending.has(sessionId)) {
      console.log(`[DEBUG TerminalManager] getOrCreate(${sessionId}) - waiting on pending`);
      return this.pending.get(sessionId)!;
    }
    // Start new creation and track the promise
    console.log(`[DEBUG TerminalManager] getOrCreate(${sessionId}) - creating NEW instance`);
    const promise = this.doCreate(sessionId);
    this.pending.set(sessionId, promise);
    try {
      const result = await promise;
      return result;
    } finally {
      this.pending.delete(sessionId);
    }
  }

  private async doCreate(sessionId: number): Promise<TerminalInstance | null> {
    try {
      console.log(`[TerminalManager] Creating terminal for session ${sessionId}`);
      const term = new Terminal({
        theme: {
          background: '#0f0f0f',
          foreground: '#e0e0e0',
          cursor: '#3b82f6',
          selectionBackground: 'rgba(59, 130, 246, 0.3)'
        },
        fontSize: 10,  // 75% of standard 13px (13 * 0.75 ≈ 10)
        fontFamily: 'Cascadia Code, Consolas, monospace',
        scrollback: 10000,
        cursorBlink: true,
        allowProposedApi: true
      });

      const fitAddon = new FitAddon();
      term.loadAddon(fitAddon);

      const unlisten = await listen<{ session_id: number; line: string }>('agent-output', (event) => {
        if (event.payload.session_id === sessionId) {
          const inst = this.instances.get(sessionId);
          if (inst) {
            inst.writeBuffer += event.payload.line;
            this.scheduleFlush(sessionId);
          }
        }
      });

      term.onData((data) => {
        invoke('write_to_agent', { sessionId, data }).catch(console.error);
      });

      term.onResize(({ cols, rows }) => {
        console.log(`[TerminalManager] Resizing session ${sessionId} to ${cols}x${rows}`);
        invoke('resize_agent', { sessionId, rows, cols }).catch(err => {
          // Ignore "Agent not running" errors - these are common during initial mount or rapid resizing
          if (err !== 'Agent not running') {
            console.error(`[TerminalManager] Resize failed for session ${sessionId}:`, err);
          }
        });
      });

      const instance: TerminalInstance = { term, fitAddon, unlisten, opened: false, writeBuffer: '', frameRequested: false };
      this.instances.set(sessionId, instance);
      this.notify();
      return instance;
    } catch (e) {
      console.error(`[TerminalManager] Failed to create terminal for ${sessionId}`, e);
      return null;
    }
  }

  private scheduleFlush(sessionId: number) {
    const inst = this.instances.get(sessionId);
    if (!inst || inst.frameRequested) return;
    inst.frameRequested = true;
    requestAnimationFrame(() => {
      // Guard against stale closures — only write if sessionId still maps to same instance
      const current = this.instances.get(sessionId);
      if (current === inst && inst.writeBuffer) {
        inst.term.write(inst.writeBuffer);
        inst.writeBuffer = '';
      }
      inst.frameRequested = false;
    });
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

// Expose terminal manager globally for E2E testing
declare global {
  interface Window {
    __terminalManager?: typeof terminalManager;
  }
}
window.__terminalManager = terminalManager;

export function AgentTerminal({ sessionId }: { sessionId: number }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const instanceRef = useRef<TerminalInstance | null>(null);
  const spawnAgent = useSessionStore(state => state.spawnAgent);
  const sessions = useSessionStore(state => state.sessions);
  const session = sessions.find(s => s.id === sessionId);

  // ResizeObserver for automatic fitting when container size changes
  useEffect(() => {
    if (!containerRef.current) return;

    const observer = new ResizeObserver(() => {
      if (instanceRef.current) {
        requestAnimationFrame(() => {
          // H1 Fix: Explicitly call CharSizeService.measure() to ensure cell dimensions are populated
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          const charSizeService = (instanceRef.current?.term as any)['_core']?.['_charSizeService'];
          if (charSizeService) {
            charSizeService.measure();
          }
          instanceRef.current?.fitAddon.fit();
        });
      }
    });

    observer.observe(containerRef.current);
    return () => observer.disconnect();
  }, [sessionId]);

  useEffect(() => {
    const cancelled = { current: false };

    terminalManager.getOrCreate(sessionId).then(inst => {
      if (cancelled.current) return;
      if (!inst || !containerRef.current) return;

      instanceRef.current = inst;

      if (!inst.opened) {
        inst.opened = true;
        inst.term.open(containerRef.current);
      } else {
        const termEl = inst.term.element;
        if (termEl && termEl.parentElement !== containerRef.current) {
          containerRef.current.appendChild(termEl);
        }
      }

      // Initial fit and check if we need to spawn the agent
      requestAnimationFrame(async () => {
        if (cancelled.current || !inst) return;
        
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        const charSizeService = (inst.term as any)['_core']?.['_charSizeService'];
        charSizeService?.measure();
        
        inst.fitAddon.fit();
        inst.term.scrollToBottom();
        inst.term.refresh(0, inst.term.rows - 1);

        // If session is idle and has a provider, spawn the agent with current dimensions
        if (session && session.provider && session.status === 'idle' && !cancelled.current) {
          const dims = inst.fitAddon.proposeDimensions();
          console.log(`[AgentTerminal] Auto-spawning agent for session ${sessionId} with dims:`, dims);
          try {
            await spawnAgent(sessionId, session.provider, dims?.rows, dims?.cols);
          } catch (e) {
            console.error('[AgentTerminal] Failed to auto-spawn agent:', e);
          }
        }
      });
    });

    return () => {
      cancelled.current = true;
    };
  }, [sessionId, session?.status]);

  return (
    <div
      ref={containerRef}
      className="h-full w-full"
      style={{ padding: '4px' }}
    />
  );
}


