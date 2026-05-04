import { useEffect, useRef, useState } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { useAgentNodeStore } from '../../stores/agentNodeStore';

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

  // Exposed for TerminalStack to trigger focus/fit on active node
  getInstance(nodeId: number): TerminalInstance | undefined {
    return this.instances.get(nodeId);
  }

  async getOrCreate(nodeId: number): Promise<TerminalInstance | null> {
    // Return existing instance immediately if available
    if (this.instances.has(nodeId)) {
      console.log(`[DEBUG TerminalManager] getOrCreate(${nodeId}) - returning existing instance`);
      return this.instances.get(nodeId)!;
    }
    // If creation is already in progress, wait for it
    if (this.pending.has(nodeId)) {
      console.log(`[DEBUG TerminalManager] getOrCreate(${nodeId}) - waiting on pending`);
      return this.pending.get(nodeId)!;
    }
    // Start new creation and track the promise
    console.log(`[DEBUG TerminalManager] getOrCreate(${nodeId}) - creating NEW instance`);
    const promise = this.doCreate(nodeId);
    this.pending.set(nodeId, promise);
    try {
      const result = await promise;
      return result;
    } finally {
      this.pending.delete(nodeId);
    }
  }

  private async doCreate(nodeId: number): Promise<TerminalInstance | null> {
    try {
      console.log(`[TerminalManager] Creating terminal for node ${nodeId}`);
      const term = new Terminal({
        theme: {
          background: '#09090f',
          foreground: '#e2e8f0',
          cursor: '#00d4ff',
          selectionBackground: 'rgba(0, 212, 255, 0.15)'
        },
        fontSize: 10,  // 75% of standard 13px (13 * 0.75 ≈ 10)
        fontFamily: 'JetBrains Mono, Fira Code, Cascadia Code, Consolas, monospace',
        fontWeight: 500,
        scrollback: 10000,
        cursorBlink: true,
        allowProposedApi: true
      });

      const fitAddon = new FitAddon();
      term.loadAddon(fitAddon);

      const unlisten = await listen<{ session_id: number; line: string }>('agent-output', (event) => {
        if (event.payload.session_id === nodeId) {
          const inst = this.instances.get(nodeId);
          if (inst) {
            inst.writeBuffer += event.payload.line;
            this.scheduleFlush(nodeId);
          }
        }
      });

      term.onData((data) => {
        invoke('write_to_agent', { sessionId: nodeId, data }).catch(console.error);
      });

      // Handle Ctrl+V / Cmd+V paste explicitly using the system clipboard API.
      // xterm.js v6 handles paste via the textarea's paste event, which on Windows can
      // fail to fire correctly under ConPTY focus management. Using the browser's
      // navigator.clipboard API gives us reliable cross-platform paste semantics.
      // We route through send_to_agent (not write_to_agent) so that pasted input
      // triggers LAST_INPUT_TIME suppression and attention-cleared events — same as
      // typed input — and appends \n since paste is a user submit action.
      term.element?.addEventListener('paste', async (e: ClipboardEvent) => {
        e.preventDefault();
        e.stopPropagation();
        const text = e.clipboardData
          ? e.clipboardData.getData('text/plain')
          : await navigator.clipboard.readText().catch(() => '');
        if (text) {
          invoke('send_to_agent', { session_id: nodeId, input: text }).catch(console.error);
        }
      });

      term.onResize(({ cols, rows }) => {
        console.log(`[TerminalManager] Resizing node ${nodeId} to ${cols}x${rows}`);
        invoke('resize_agent', { sessionId: nodeId, rows, cols }).catch(err => {
          // Ignore "Agent not running" errors - these are common during initial mount or rapid resizing
          if (err !== 'Agent not running') {
            console.error(`[TerminalManager] Resize failed for node ${nodeId}:`, err);
          }
        });
      });

      const instance: TerminalInstance = { term, fitAddon, unlisten, opened: false, writeBuffer: '', frameRequested: false };
      this.instances.set(nodeId, instance);
      this.notify();
      return instance;
    } catch (e) {
      console.error(`[TerminalManager] Failed to create terminal for ${nodeId}`, e);
      return null;
    }
  }

  private scheduleFlush(nodeId: number) {
    const inst = this.instances.get(nodeId);
    if (!inst || inst.frameRequested) return;
    inst.frameRequested = true;
    requestAnimationFrame(() => {
      // Guard against stale closures — only write if nodeId still maps to same instance
      const current = this.instances.get(nodeId);
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

  dispose(nodeId: number) {
    const instance = this.instances.get(nodeId);
    if (instance) {
      instance.unlisten();
      instance.term.dispose();
      this.instances.delete(nodeId);
      this.notify();
    }
  }
}

export const terminalManager = new TerminalManager();

/**
 * Backward compatibility export
 */
export function disposeTerminal(nodeId: number) {
  terminalManager.dispose(nodeId);
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
  const [isDragging, setIsDragging] = useState(false);
  const spawnAgent = useAgentNodeStore(state => state.spawnAgent);
  const agentNodes = useAgentNodeStore(state => state.agentNodes);
  const node = agentNodes.find(s => s.id === sessionId);

  const handleDragOver = (e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setIsDragging(true);
  };

  const handleDragLeave = (e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setIsDragging(false);
  };

  const handleDrop = async (e: React.DragEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setIsDragging(false);

    const files = Array.from(e.dataTransfer.files);
    if (files.length === 0) return;

    const inst = instanceRef.current;
    if (!inst) return;

    const paths = await Promise.all(
      files.map(async (file) => {
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        const filePath = (file as any).path as string;
        if (!filePath) return null;
        return invoke<string>('to_host_path', { path: filePath });
      })
    );
    for (const absPath of paths) {
      if (absPath) inst.term.write(absPath);
    }
  };

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

        // If node is idle and has a provider, spawn the agent with current dimensions
        if (node && node.provider && node.status === 'idle' && !cancelled.current) {
          const dims = inst.fitAddon.proposeDimensions();
          console.log(`[AgentTerminal] Auto-spawning agent for node ${sessionId} with dims:`, dims);
          try {
            await spawnAgent(sessionId, node.provider, dims?.rows, dims?.cols);
          } catch (e) {
            console.error('[AgentTerminal] Failed to auto-spawn agent:', e);
          }
        }
      });
    });

    return () => {
      cancelled.current = true;
    };
  }, [sessionId, node?.status]);

  return (
    <div
      ref={containerRef}
      className="h-full w-full relative"
      style={{ padding: '4px' }}
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
    >
      {isDragging && (
        <div className="absolute inset-0 bg-cyan-500/10 border-2 border-dashed border-cyan-500 rounded-lg flex items-center justify-center z-50 pointer-events-none">
          <span className="text-cyan-400 text-sm font-medium">Drop file to paste path</span>
        </div>
      )}
    </div>
  );
}