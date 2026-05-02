import { useEffect, useRef, useState } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { FIT_TERMINALS } from '../../lib/events';

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

function TerminalContainer({ sessionId, isVisible }: { sessionId: number; isVisible: boolean }) {
  const containerRef = useRef<HTMLDivElement>(null);
  const instanceRef = useRef<TerminalInstance | null>(null);

  useEffect(() => {
    console.log(`[DEBUG TerminalContainer] Effect started for session ${sessionId}`);

    // H3 Fix: Use object ref instead of primitive boolean to avoid closure capture race
    const cancelled = { current: false };

    terminalManager.getOrCreate(sessionId).then(inst => {
      if (cancelled.current) return;
      if (!inst || !containerRef.current) {
        console.log(`[DEBUG TerminalContainer] session ${sessionId}: no instance or no container`);
        return;
      }

      instanceRef.current = inst;

      if (!inst.opened) {
        console.log(`[DEBUG TerminalContainer] session ${sessionId}: calling open()`);
        inst.opened = true;
        inst.term.open(containerRef.current);
      } else {
        // Re-parent terminal element into this container if it was mounted elsewhere
        const termEl = inst.term.element;
        if (termEl && termEl.parentElement !== containerRef.current) {
          console.log(`[DEBUG TerminalContainer] session ${sessionId}: re-parenting terminal element`);
          containerRef.current.appendChild(termEl);
        }
      }

      // Always do fit/refresh when effect runs - use rAF to ensure DOM dimensions are computed
      requestAnimationFrame(() => {
        if (cancelled.current || !inst) return;
        console.log(`[DEBUG TerminalContainer] session ${sessionId}: fit/refresh via rAF`);
        const dims = inst.fitAddon.proposeDimensions();
        console.log(`[DEBUG TerminalContainer] session ${sessionId}: proposeDimensions=${JSON.stringify(dims)}, actual rows=${inst.term.rows}, cols=${inst.term.cols}`);
        // Force a resize from container bounding rect - works regardless of proposeDimensions state
        if (containerRef.current) {
          const rect = containerRef.current.getBoundingClientRect();
          const cols = Math.floor((rect.width - 14) / 8.16);
          const rows = Math.floor((rect.height - 14) / 17);
          console.log(`[DEBUG TerminalContainer] session ${sessionId}: forcing resize cols=${cols}, rows=${rows} from rect ${rect.width}x${rect.height}`);
          inst.term.resize(cols, rows);
          inst.fitAddon.fit();
          const dimsAfter = inst.fitAddon.proposeDimensions();
          console.log(`[DEBUG TerminalContainer] session ${sessionId}: after resize proposeDimensions=${JSON.stringify(dimsAfter)}`);
        } else {
          console.log(`[DEBUG TerminalContainer] session ${sessionId}: no container ref, skipping`);
        }
        inst.term.scrollToBottom();
        inst.term.refresh(0, inst.term.rows - 1);
      });
    });

    return () => {
      console.log(`[DEBUG TerminalContainer] Cleanup for session ${sessionId}`);
      cancelled.current = true;
    };
  }, [sessionId]);

  // Re-fit when visibility changes - always fit when visible, with manual resize fallback
  useEffect(() => {
    console.log(`[DEBUG TerminalContainer] Visibility effect for ${sessionId}, isVisible=${isVisible}`);
    if (isVisible && instanceRef.current) {
      // H5 Fix: Use double rAF to ensure element is fully painted before measuring
      requestAnimationFrame(() => {
        requestAnimationFrame(() => {
          if (!instanceRef.current || !containerRef.current) return;
          console.log(`[DEBUG TerminalContainer] ${sessionId}: visibility=true, doing H1 charSizeService.measure() + fit via double rAF`);

          // H1 Fix: Explicitly call CharSizeService.measure() to ensure cell dimensions are populated
          // Using any assertion because _core is private internal API
          // eslint-disable-next-line @typescript-eslint/no-explicit-any
          const charSizeService = (instanceRef.current.term as any)['_core']?.['_charSizeService'];
          if (charSizeService) {
            console.log(`[DEBUG TerminalContainer] ${sessionId}: calling charSizeService.measure()`);
            charSizeService.measure();
          } else {
            console.log(`[DEBUG TerminalContainer] ${sessionId}: charSizeService not found on _core`);
          }

          const dims = instanceRef.current.fitAddon.proposeDimensions();
          console.log(`[DEBUG TerminalContainer] ${sessionId}: pre-fit proposeDimensions=${JSON.stringify(dims)}`);

          const rect = containerRef.current.getBoundingClientRect();
          const cols = Math.floor((rect.width - 14) / 8.16);
          const rows = Math.floor((rect.height - 14) / 17);
          console.log(`[DEBUG TerminalContainer] ${sessionId}: visibility resize cols=${cols}, rows=${rows} from rect ${rect.width}x${rect.height}`);
          instanceRef.current.term.resize(cols, rows);
          instanceRef.current.fitAddon.fit();

          const dimsAfter = instanceRef.current.fitAddon.proposeDimensions();
          console.log(`[DEBUG TerminalContainer] ${sessionId}: post-fit proposeDimensions=${JSON.stringify(dimsAfter)}`);
          instanceRef.current.term.focus();
          instanceRef.current.term.scrollToBottom();
          instanceRef.current.term.refresh(0, instanceRef.current.term.rows - 1);
        });
      });
    }
  }, [isVisible]);

  return (
    <div
      ref={containerRef}
      className={`h-full w-full ${isVisible ? '' : 'opacity-0 pointer-events-none'}`}
      style={{ padding: '4px' }}
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

  // Listen for FIT_TERMINALS events and trigger fit on the active terminal
  useEffect(() => {
    console.log(`[DEBUG TerminalStack] FIT_TERMINALS listener registered`);
    const unlisten = listen<{ sessionId: number }>(FIT_TERMINALS, (event) => {
      const sessionId = event.payload.sessionId;
      console.log(`[DEBUG TerminalStack] FIT_TERMINALS received for session ${sessionId}`);
      const inst = terminalManager.getInstance(sessionId);
      console.log(`[DEBUG TerminalStack] inst exists=${!!inst}, opened=${inst?.opened}`);
      if (inst && inst.opened) {
        console.log(`[DEBUG TerminalStack] Fitting terminal ${sessionId}`);
        inst.fitAddon.fit();
        inst.term.focus();
        inst.term.scrollToBottom();
        inst.term.refresh(0, inst.term.rows - 1);
      } else {
        console.log(`[DEBUG TerminalStack] Cannot fit terminal ${sessionId} - inst=${!!inst}, opened=${inst?.opened}`);
      }
    });
    return () => { unlisten.then(fn => fn()); };
  }, []);

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
