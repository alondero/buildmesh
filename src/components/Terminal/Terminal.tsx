import { useEffect, useRef, useState, useCallback, type WheelEvent as ReactWheelEvent } from 'react';
import '@xterm/xterm/css/xterm.css';
import { invoke } from '@tauri-apps/api/core';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { terminalFontSize, setTerminalFontSize, TERMINAL_FONT_SIZE_DEFAULT, SEARCH_DECORATIONS } from './terminalConfig';
import { isMac } from '../../lib/platform';
import { TerminalRegistry, type TerminalInstance } from './TerminalRegistry';

export { type TerminalInstance } from './TerminalRegistry';
export const terminalManager = new TerminalRegistry();

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
  const [handoverProviderLabel, setHandoverProviderLabel] = useState<string | null>(null);
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

  const handleHandover = async () => {
    const inst = terminalManager.getInstance(sessionId);
    if (!inst || !inst.term.hasSelection()) { setContextMenu(null); return; }
    const selection = inst.term.getSelection();
    if (!selection.trim()) { setContextMenu(null); return; }
    if (!node) { setContextMenu(null); return; }
    try {
      await useAgentNodeStore.getState().spawnHandoverAgent(
        node.mesh_id, selection, undefined,
      );
    } catch (e) {
      console.error('[AgentTerminal] handover failed:', e);
    }
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

  // Pre-fetch the default provider label for the handover menu item
  useEffect(() => {
    if (!node) return;
    invoke<string>('get_default_provider', { meshId: node.mesh_id })
      .then(async (defProvider) => {
        const providers = await invoke<{ id: string; label: string }[]>('list_providers');
        const info = providers.find(p => p.id === defProvider);
        setHandoverProviderLabel(info?.label ?? defProvider);
      })
      .catch(() => setHandoverProviderLabel('Default'));
  }, [node?.mesh_id]);

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
          {handoverProviderLabel !== null && (
            <>
              <div className="border-t border-border-default my-0.5" />
              <button
                onClick={handleHandover}
                disabled={!instRef.current?.term.hasSelection()}
                className="w-full px-3 py-1.5 text-left text-[11px] text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors disabled:text-text-muted disabled:cursor-default disabled:hover:bg-transparent"
              >
                Handover to new Node [{handoverProviderLabel}]
              </button>
            </>
          )}
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
