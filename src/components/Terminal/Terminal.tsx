import { useEffect, useRef, useState, useCallback, type WheelEvent as ReactWheelEvent } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { SerializeAddon } from '@xterm/addon-serialize';
import { SearchAddon } from '@xterm/addon-search';
import { WebLinksAddon } from '@xterm/addon-web-links';
import '@xterm/xterm/css/xterm.css';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { openUrl } from '@tauri-apps/plugin-opener';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { createTerminalOptions, terminalFontSize, setTerminalFontSize, onTerminalFontSizeChange, TERMINAL_FONT_SIZE_DEFAULT, SEARCH_DECORATIONS } from './terminalConfig';
import { resolveKeyAction } from './terminalKeyAction';
import { isMac } from '../../lib/platform';

interface TerminalInstance {
  term: Terminal;
  fitAddon: FitAddon;
  serializeAddon: SerializeAddon;
  searchAddon: SearchAddon;
  unlisten: UnlistenFn;
  opened: boolean;
  writeBuffer: string;
  frameRequested: boolean;
  resizeObserver: ResizeObserver | null;
  attachedContainer: HTMLElement | null;
  onFindRequest: (() => void) | null;
}

class TerminalManager {
  private instances = new Map<number, TerminalInstance>();
  private listeners = new Set<() => void>();
  private pending = new Map<number, Promise<TerminalInstance | null>>();
  private fontSizeUnlisten: () => void;

  constructor() {
    this.fontSizeUnlisten = onTerminalFontSizeChange((size) => {
      for (const inst of this.instances.values()) {
        inst.term.options.fontSize = size;
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        const charSizeService = (inst.term as any)['_core']?.['_charSizeService'];
        charSizeService?.measure();
        inst.fitAddon.fit();
      }
    });
  }

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
   * Inverse of attach: disconnects the ResizeObserver and removes the terminal's
   * DOM element from its parent. The element removal is what prevents stacking
   * when the same container node is reused for a different sessionId.
   */
  detach(nodeId: number): void {
    const inst = this.instances.get(nodeId);
    if (!inst) return;

    if (inst.resizeObserver) {
      inst.resizeObserver.disconnect();
      inst.resizeObserver = null;
    }
    inst.term.element?.remove();
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
      const term = new Terminal(createTerminalOptions());

      const fitAddon = new FitAddon();
      term.loadAddon(fitAddon);
      const serializeAddon = new SerializeAddon();
      term.loadAddon(serializeAddon);
      const searchAddon = new SearchAddon();
      term.loadAddon(searchAddon);
      const webLinksAddon = new WebLinksAddon((_event, uri) => {
        openUrl(uri).catch(console.error);
      });
      term.loadAddon(webLinksAddon);

      const unlisten = await listen<{ session_id: number; line: string }>('agent-output', (event) => {
        if (event.payload.session_id === nodeId) {
          const inst = this.instances.get(nodeId);
          if (inst) {
            inst.writeBuffer += event.payload.line;
            this.scheduleFlush(nodeId);
          }
        }
      });

      const unlistenSerialize = await listen<{ node_id: number; request_id: string }>('serialize-terminal-request', (event) => {
        if (event.payload.node_id === nodeId) {
          const inst = this.instances.get(nodeId);
          if (inst) {
            const snapshot = inst.serializeAddon.serialize({ scrollback: 200 });
            invoke('submit_terminal_snapshot', {
              requestId: event.payload.request_id,
              data: snapshot,
            }).catch(console.error);
          }
        }
      });

      term.onData((data) => {
        invoke('write_to_agent', { sessionId: nodeId, data }).catch(console.error);
      });

      term.attachCustomKeyEventHandler((ev: KeyboardEvent) => {
        if (ev.type !== 'keydown') return true;

        const action = resolveKeyAction({
          key: ev.key,
          ctrlKey: ev.ctrlKey,
          shiftKey: ev.shiftKey,
          metaKey: ev.metaKey,
          isMac,
          hasSelection: term.hasSelection(),
        });

        const inst = this.instances.get(nodeId);
        switch (action) {
          case 'copy':
            navigator.clipboard.writeText(term.getSelection()).catch(err => {
              console.warn('[TerminalManager] Clipboard write failed:', err);
            });
            return false;
          case 'paste':
            ev.preventDefault();
            navigator.clipboard.readText().then(text => {
              if (text) term.paste(text);
            }).catch(err => {
              console.warn('[TerminalManager] Clipboard read failed:', err);
            });
            return false;
          case 'selectAll':
            term.selectAll();
            return false;
          case 'find':
            inst?.onFindRequest?.();
            return false;
          case 'clear':
            term.clear();
            return false;
          case 'passthrough':
            return true;
        }
      });

      term.onResize(({ cols, rows }) => {
        invoke('resize_agent', { sessionId: nodeId, rows, cols }).catch(err => {
          // Ignore "Agent not running" errors - these are common during initial mount or rapid resizing
          if (err !== 'Agent not running') {
            console.error(`[TerminalManager] Resize failed for node ${nodeId}:`, err);
          }
        });
      });

      const combinedUnlisten: UnlistenFn = () => { unlisten(); unlistenSerialize(); };
      const instance: TerminalInstance = {
        term,
        fitAddon,
        serializeAddon,
        searchAddon,
        unlisten: combinedUnlisten,
        opened: false,
        writeBuffer: '',
        frameRequested: false,
        resizeObserver: null,
        attachedContainer: null,
        onFindRequest: null,
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

  destroy() {
    this.fontSizeUnlisten();
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
  const instRef = useRef<TerminalInstance | null>(null);
  const [isDragging, setIsDragging] = useState(false);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState('');
  const searchInputRef = useRef<HTMLInputElement>(null);
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

  const handleWheel = useCallback((e: ReactWheelEvent<HTMLDivElement>) => {
    if (e.ctrlKey) {
      e.preventDefault();
      const delta = e.deltaY < 0 ? 2 : -2;
      setTerminalFontSize(terminalFontSize() + delta);
    }
  }, []);

  const handleContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setContextMenu({ x: e.clientX, y: e.clientY });
  };

  const handleCopy = () => {
    const inst = terminalManager.getInstance(sessionId);
    if (inst && inst.term.hasSelection()) {
      navigator.clipboard.writeText(inst.term.getSelection()).catch(console.error);
    }
    setContextMenu(null);
  };

  const handlePaste = () => {
    const inst = terminalManager.getInstance(sessionId);
    if (inst) {
      navigator.clipboard.readText().then(text => {
        if (text) inst.term.paste(text);
      }).catch(console.error);
    }
    setContextMenu(null);
  };

  const handleSelectAll = () => {
    const inst = terminalManager.getInstance(sessionId);
    if (inst) inst.term.selectAll();
    setContextMenu(null);
  };

  const handleFind = () => {
    setContextMenu(null);
    setSearchOpen(true);
  };

  const handleClear = () => {
    const inst = terminalManager.getInstance(sessionId);
    if (inst) inst.term.clear();
    setContextMenu(null);
  };

  // Dismiss context menu
  useEffect(() => {
    if (!contextMenu) return;
    const handleClick = () => setContextMenu(null);
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setContextMenu(null);
    };
    document.addEventListener('mousedown', handleClick);
    document.addEventListener('keydown', handleKeyDown);
    return () => {
      document.removeEventListener('mousedown', handleClick);
      document.removeEventListener('keydown', handleKeyDown);
    };
  }, [contextMenu]);

  // Focus search input when opened
  useEffect(() => {
    if (searchOpen) {
      requestAnimationFrame(() => searchInputRef.current?.focus());
    } else {
      setSearchQuery('');
      const inst = terminalManager.getInstance(sessionId);
      if (inst) inst.searchAddon.clearDecorations();
    }
  }, [searchOpen, sessionId]);

  const handleSearchChange = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
    const query = e.target.value;
    setSearchQuery(query);
    const inst = terminalManager.getInstance(sessionId);
    if (!inst) return;
    if (query) {
      inst.searchAddon.findNext(query, { incremental: true, decorations: SEARCH_DECORATIONS });
    } else {
      inst.searchAddon.clearDecorations();
    }
  }, [sessionId]);

  const handleSearchNext = useCallback(() => {
    const inst = terminalManager.getInstance(sessionId);
    if (inst && searchQuery) {
      inst.searchAddon.findNext(searchQuery, { decorations: SEARCH_DECORATIONS });
    }
  }, [sessionId, searchQuery]);

  const handleSearchPrev = useCallback(() => {
    const inst = terminalManager.getInstance(sessionId);
    if (inst && searchQuery) {
      inst.searchAddon.findPrevious(searchQuery, { decorations: SEARCH_DECORATIONS });
    }
  }, [sessionId, searchQuery]);

  const handleSearchKeyDown = useCallback((e: React.KeyboardEvent) => {
    if (e.key === 'Escape') {
      setSearchOpen(false);
      instRef.current?.term.focus();
    } else if (e.key === 'Enter') {
      e.preventDefault();
      if (e.shiftKey) {
        handleSearchPrev();
      } else {
        handleSearchNext();
      }
    }
  }, [handleSearchNext, handleSearchPrev]);

  // Keyboard shortcuts: Ctrl+0 reset, Ctrl++ zoom in, Ctrl+- zoom out
  useEffect(() => {
    const handler = (e: KeyboardEvent) => {
      if (!e.ctrlKey) return;
      if (e.key === '0') {
        e.preventDefault();
        setTerminalFontSize(TERMINAL_FONT_SIZE_DEFAULT);
      } else if (e.key === '=' || e.key === '+') {
        e.preventDefault();
        setTerminalFontSize(terminalFontSize() + 2);
      } else if (e.key === '-' || e.key === '_') {
        e.preventDefault();
        setTerminalFontSize(terminalFontSize() - 2);
      }
    };
    window.addEventListener('keydown', handler);
    return () => window.removeEventListener('keydown', handler);
  }, []);

  useEffect(() => {
    if (sessionId === activeNodeId && instRef.current) {
      instRef.current.term.focus();
    }
  }, [activeNodeId, sessionId]);

  useEffect(() => {
    if (!containerRef.current) return;
    const cancelled = { current: false };
    const container = containerRef.current;

    terminalManager.attach(sessionId, container).then(async (inst) => {
      if (cancelled.current || !inst) return;
      instRef.current = inst;
      inst.onFindRequest = () => setSearchOpen(true);

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
      const inst = terminalManager.getInstance(sessionId);
      if (inst) inst.onFindRequest = null;
      terminalManager.detach(sessionId);
    };
  }, [sessionId, node?.status]);

  return (
    <div
      ref={containerRef}
      className="h-full w-full relative outline-none"
      style={{ padding: '4px' }}
      tabIndex={0}
      onFocus={() => instRef.current?.term.focus()}
      onKeyDown={(e) => {
        if (e.key === 'Tab' && !e.shiftKey && !e.altKey && !e.metaKey) {
          e.preventDefault();
          instRef.current?.term.focus();
        }
      }}
      onContextMenu={handleContextMenu}
      onDragOver={handleDragOver}
      onDragLeave={handleDragLeave}
      onDrop={handleDrop}
      onWheel={handleWheel}
    >
      {isDragging && (
        <div className="absolute inset-0 bg-cyan-500/10 border-2 border-dashed border-cyan-500 rounded-lg flex items-center justify-center z-50 pointer-events-none">
          <span className="text-cyan-400 text-sm font-medium">Drop file to paste path</span>
        </div>
      )}

      {searchOpen && (
        <div className="absolute top-1 right-1 z-50 flex items-center gap-1 bg-bg-card border border-border-default rounded px-2 py-1 shadow-lg">
          <input
            ref={searchInputRef}
            type="text"
            value={searchQuery}
            onChange={handleSearchChange}
            onKeyDown={handleSearchKeyDown}
            placeholder="Find..."
            className="bg-transparent text-[11px] text-text-primary outline-none w-36 placeholder:text-text-muted"
          />
          <button onClick={handleSearchPrev} className="text-text-muted hover:text-accent-cyan text-[11px] px-1" title="Previous (Shift+Enter)">&#9650;</button>
          <button onClick={handleSearchNext} className="text-text-muted hover:text-accent-cyan text-[11px] px-1" title="Next (Enter)">&#9660;</button>
          <button onClick={() => { setSearchOpen(false); instRef.current?.term.focus(); }} className="text-text-muted hover:text-accent-cyan text-[11px] px-1" title="Close (Esc)">&#10005;</button>
        </div>
      )}

      {contextMenu && (
        <div
          className="fixed bg-bg-card border border-border-default rounded shadow-lg z-[100] py-1 min-w-[160px]"
          style={{ top: contextMenu.y, left: contextMenu.x }}
          onMouseDown={(e) => e.stopPropagation()}
        >
          <button
            onClick={handleCopy}
            disabled={!instRef.current?.term.hasSelection()}
            className="w-full px-3 py-1.5 text-left text-[11px] text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors disabled:text-text-muted disabled:cursor-default disabled:hover:bg-transparent"
          >
            Copy <span className="float-right text-text-muted">{isMac ? '⌘C' : 'Ctrl+C'}</span>
          </button>
          <button
            onClick={handlePaste}
            className="w-full px-3 py-1.5 text-left text-[11px] text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            Paste <span className="float-right text-text-muted">{isMac ? '⌘V' : 'Ctrl+V'}</span>
          </button>
          <div className="border-t border-border-default my-0.5" />
          <button
            onClick={handleSelectAll}
            className="w-full px-3 py-1.5 text-left text-[11px] text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            Select All <span className="float-right text-text-muted">{isMac ? '⌘A' : 'Ctrl+Shift+A'}</span>
          </button>
          <button
            onClick={handleFind}
            className="w-full px-3 py-1.5 text-left text-[11px] text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            Find... <span className="float-right text-text-muted">{isMac ? '⌘F' : 'Ctrl+Shift+F'}</span>
          </button>
          <div className="border-t border-border-default my-0.5" />
          <button
            onClick={handleClear}
            className="w-full px-3 py-1.5 text-left text-[11px] text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            Clear Terminal <span className="float-right text-text-muted">{isMac ? '⌘K' : 'Ctrl+Shift+K'}</span>
          </button>
        </div>
      )}
    </div>
  );
}
