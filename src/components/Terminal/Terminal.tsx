import { useEffect, useRef, useState } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { TERMINAL_OPTIONS } from './terminalConfig';

interface TerminalInstance {
  term: Terminal;
  fitAddon: FitAddon;
  unlisten: UnlistenFn;
  opened: boolean;
  writeBuffer: string;
  frameRequested: boolean;
  resizeObserver: ResizeObserver | null;
  attachedContainer: HTMLElement | null;
}

class TerminalManager {
  private instances = new Map<number, TerminalInstance>();
  private listeners = new Set<() => void>();
  private pending = new Map<number, Promise<TerminalInstance | null>>();

  /**
   * Returns the raw TerminalInstance if it exists (escape hatch).
   */
  getInstance(nodeId: number): TerminalInstance | undefined {
    return this.instances.get(nodeId);
  }

  /**
   * Returns the underlying xterm.js Terminal for direct access if needed.
   */
  getTerminal(nodeId: number): Terminal | undefined {
    return this.instances.get(nodeId)?.term;
  }

  /**
   * Attach a terminal to a visible DOM container. Creates the terminal instance
   * if it doesn't exist yet, opens or reparents it into the container, sets up
   * a ResizeObserver for auto-fitting, and subscribes to agent-output events.
   */
  async attach(nodeId: number, container: HTMLElement): Promise<TerminalInstance | null> {
    const inst = await this.getOrCreate(nodeId);
    if (!inst) return null;

    // Open into the container (first time) or reparent (subsequent mounts)
    if (!inst.opened) {
      inst.opened = true;
      inst.term.open(container);
    } else {
      const termEl = inst.term.element;
      if (termEl && termEl.parentElement !== container) {
        container.appendChild(termEl);
      }
    }

    inst.attachedContainer = container;

    // Set up ResizeObserver for this container
    if (inst.resizeObserver) {
      inst.resizeObserver.disconnect();
    }
    inst.resizeObserver = new ResizeObserver(() => {
      requestAnimationFrame(() => {
        if (!inst.attachedContainer) return;
        // Ensure char dimensions are populated before fitting
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        const charSizeService = (inst.term as any)['_core']?.['_charSizeService'];
        if (charSizeService) {
          charSizeService.measure();
        }
        inst.fitAddon.fit();
      });
    });
    inst.resizeObserver.observe(container);

    // Initial fit, scroll, and refresh
    requestAnimationFrame(() => {
      // eslint-disable-next-line @typescript-eslint/no-explicit-any
      const charSizeService = (inst.term as any)['_core']?.['_charSizeService'];
      charSizeService?.measure();
      inst.fitAddon.fit();
      inst.term.scrollToBottom();
      inst.term.refresh(0, inst.term.rows - 1);
    });

    return inst;
  }

  /**
   * Detach a terminal from its visible container. Disconnects the ResizeObserver
   * but preserves the terminal instance for later reattachment.
   */
  detach(nodeId: number): void {
    const inst = this.instances.get(nodeId);
    if (!inst) return;

    if (inst.resizeObserver) {
      inst.resizeObserver.disconnect();
      inst.resizeObserver = null;
    }
    inst.attachedContainer = null;
  }

  /**
   * Trigger a fit for a specific terminal.
   */
  fit(nodeId: number): void {
    const inst = this.instances.get(nodeId);
    if (!inst) return;
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const charSizeService = (inst.term as any)['_core']?.['_charSizeService'];
    charSizeService?.measure();
    inst.fitAddon.fit();
  }

  /**
   * Fit all currently attached terminals.
   */
  fitAll(): void {
    for (const [nodeId, inst] of this.instances) {
      if (inst.attachedContainer) {
        this.fit(nodeId);
      }
    }
  }

  /**
   * Write data to a specific terminal.
   */
  write(nodeId: number, data: string): void {
    const inst = this.instances.get(nodeId);
    if (!inst) return;
    inst.term.write(data);
  }

  async getOrCreate(nodeId: number): Promise<TerminalInstance | null> {
    // Return existing instance immediately if available
    if (this.instances.has(nodeId)) {
      return this.instances.get(nodeId)!;
    }
    // If creation is already in progress, wait for it
    if (this.pending.has(nodeId)) {
      return this.pending.get(nodeId)!;
    }
    // Start new creation and track the promise
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
      const term = new Terminal(TERMINAL_OPTIONS);

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

      // attachCustomKeyEventHandler works before term.open(), unlike DOM listeners
      // that require term.element. Raw write_to_agent avoids appending \n on multi-line paste.
      let pasteInFlight = false;
      term.attachCustomKeyEventHandler((ev: KeyboardEvent) => {
        if (ev.type === 'keydown' && ev.key === 'v' && (ev.ctrlKey || ev.metaKey)) {
          if (!pasteInFlight) {
            pasteInFlight = true;
            navigator.clipboard.readText().then(text => {
              if (text) invoke('write_to_agent', { sessionId: nodeId, data: text }).catch(console.error);
            }).catch(err => {
              console.warn('[TerminalManager] Clipboard read failed:', err);
            }).finally(() => { pasteInFlight = false; });
          }
          return false;
        }
        return true;
      });

      term.onResize(({ cols, rows }) => {
        invoke('resize_agent', { sessionId: nodeId, rows, cols }).catch(err => {
          // Ignore "Agent not running" errors - these are common during initial mount or rapid resizing
          if (err !== 'Agent not running') {
            console.error(`[TerminalManager] Resize failed for node ${nodeId}:`, err);
          }
        });
      });

      const instance: TerminalInstance = {
        term,
        fitAddon,
        unlisten,
        opened: false,
        writeBuffer: '',
        frameRequested: false,
        resizeObserver: null,
        attachedContainer: null,
      };
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

  /**
   * Fully dispose a terminal instance. ONLY call this when an agent node is
   * explicitly deleted — never for view switches or session changes.
   */
  dispose(nodeId: number) {
    const instance = this.instances.get(nodeId);
    if (instance) {
      if (instance.resizeObserver) {
        instance.resizeObserver.disconnect();
      }
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
  const [isDragging, setIsDragging] = useState(false);
  const spawnAgent = useAgentNodeStore(state => state.spawnAgent);
  const agentNodes = useAgentNodeStore(state => state.agentNodes);
  const activeNodeId = useAgentNodeStore(state => state.activeNodeId);
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

    const inst = terminalManager.getInstance(sessionId);
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

  useEffect(() => {
    if (!containerRef.current) return;
    const cancelled = { current: false };
    const container = containerRef.current;

    terminalManager.attach(sessionId, container).then(async (inst) => {
      if (cancelled.current || !inst) return;

      if (sessionId === activeNodeId) {
        inst.term.focus();
      }

      // If node is idle and has a provider, spawn the agent with current dimensions
      if (node && node.provider && node.status === 'idle') {
        const dims = inst.fitAddon.proposeDimensions();
        try {
          await spawnAgent(sessionId, node.provider, dims?.rows, dims?.cols);
        } catch (e) {
          console.error('[AgentTerminal] Failed to auto-spawn agent:', e);
        }
      }
    });

    return () => {
      cancelled.current = true;
      terminalManager.detach(sessionId);
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
