import { useEffect, useRef, useState, useCallback, type WheelEvent as ReactWheelEvent } from 'react';
import '@xterm/xterm/css/xterm.css';
import { invoke } from '@tauri-apps/api/core';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useUIStore } from '../../stores/uiStore';
import { terminalFontSize, setTerminalFontSize, TERMINAL_FONT_SIZE_DEFAULT, SEARCH_DECORATIONS } from './terminalConfig';
import { isMac } from '../../lib/platform';
import { TerminalRegistry, type TerminalInstance } from './TerminalRegistry';
import { getHandoverProviderLabel } from './handoverProviderCache';

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
  const scrollDisposableRef = useRef<{ dispose: () => void } | null>(null);
  const isDragging = useUIStore(state => state.dragTargetNodeId === sessionId);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState('');
  const [atBottom, setAtBottom] = useState(true);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const [handoverProviderLabel, setHandoverProviderLabel] = useState<string | null>(null);
  const spawnAgent = useAgentNodeStore(state => state.spawnAgent);
  const activeNodeId = useAgentNodeStore(state => state.activeNodeId);
  // Subscribe to *this* node only. Selecting the whole agentNodes array would
  // re-render every open pane whenever any node's status flips (attention
  // events fire constantly while agents work); find() returns the same object
  // reference unless this specific node changed, so Zustand skips the render.
  const node = useAgentNodeStore(state => state.agentNodes.find(s => s.id === sessionId));

  // OS file drops (Explorer / Finder) are handled window-level in
  // useFileDropToTerminal — Tauri intercepts the native drop before the DOM, so
  // an element-level HTML5 onDrop never receives the files.

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
      // invoke('read_clipboard') uses pbpaste on macOS to bypass the WKWebView
      // clipboard-permission popup (macOS 14+). On other platforms it errors and
      // we fall back to the web clipboard API.
      invoke<string>('read_clipboard').then(text => {
        if (text) inst.term.paste(text);
      }).catch(() => {
        navigator.clipboard.readText().then(text => {
          if (text) inst.term.paste(text);
        }).catch(console.error);
      });
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

  // Focus guardian. In a multi-pane grid, background DOM churn — a re-render
  // that momentarily re-parents xterm's imperatively-appended `.xterm` element,
  // an overlay mounting/unmounting next to it, or a WebView2 focus hiccup — can
  // blur the terminal's hidden helper-textarea and drop keyboard focus to
  // <body> mid-keystroke, with no user action, so the next characters go
  // nowhere. When *this* node is the active one and the app window still holds
  // OS focus, reclaim it. We act ONLY when focus has fallen to nothing
  // (<body>/null): a click that moves focus to a real control (the search box, a
  // rename input, a header button, another pane) lands on that element, so the
  // guard skips and never fights a legitimate move. activeNodeId is read via
  // getState() so the listener doesn't need re-binding on every switch.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const onFocusOut = () => {
      // Defer a microtask so document.activeElement reflects where focus
      // actually landed before we judge whether it fell out entirely.
      queueMicrotask(() => {
        if (sessionId !== useAgentNodeStore.getState().activeNodeId) return;
        if (typeof document.hasFocus === 'function' && !document.hasFocus()) return;
        const active = document.activeElement;
        if (active && active !== document.body) return; // moved to a real control — leave it
        // Read the live instance from the registry (not instRef): a node closed
        // mid-flight is removed from the registry, so this skips it rather than
        // focusing a disposed xterm (dispose() doesn't null attachedContainer).
        const inst = terminalManager.getInstance(sessionId);
        if (!inst || !inst.attachedContainer) return;
        console.warn(`[AgentTerminal] keyboard focus fell to <body> while node ${sessionId} was active — restoring terminal focus`);
        inst.term.focus();
      });
    };
    container.addEventListener('focusout', onFocusOut);
    return () => container.removeEventListener('focusout', onFocusOut);
  }, [sessionId]);

  // Attach/detach the xterm element. Keyed on sessionId ONLY. It used to also
  // depend on node.status, which meant every attention flip (running ↔
  // awaiting_input, many per minute while an agent works) tore the terminal
  // element out of the DOM and re-appended it — a visible reflow and a
  // micro-hang. The status-driven auto-spawn lives in its own effect below so
  // it no longer drags the DOM lifecycle with it.
  useEffect(() => {
    if (!containerRef.current) return;
    const cancelled = { current: false };
    const container = containerRef.current;
    setAtBottom(true);

    terminalManager.attach(sessionId, container).then((inst) => {
      if (cancelled.current || !inst) return;
      instRef.current = inst;
      inst.onFindRequest = () => setSearchOpen(true);

      const updateAtBottom = () => {
        const buf = inst.term.buffer.active;
        setAtBottom(buf.viewportY >= buf.baseY);
      };
      scrollDisposableRef.current?.dispose(); // allow-dispose — scroll listener, not the xterm terminal
      scrollDisposableRef.current = inst.term.onScroll(updateAtBottom);
      updateAtBottom();

      if (sessionId === activeNodeId) {
        inst.term.focus();
      }
    });

    return () => {
      cancelled.current = true;
      const inst = terminalManager.getInstance(sessionId);
      if (inst) inst.onFindRequest = null;
      scrollDisposableRef.current?.dispose(); // allow-dispose — scroll listener, not the xterm terminal
      scrollDisposableRef.current = null;
      terminalManager.detach(sessionId);
    };
  // activeNodeId is read once at attach time for the initial focus; the
  // dedicated focus effect above handles later changes, so it's intentionally
  // not a dependency here.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [sessionId]);

  // Auto-spawn the agent for a freshly created (idle) node. Separated from the
  // attach effect so it can react to the status becoming 'idle' without
  // re-attaching the DOM. We use `attach` (not `getOrCreate`) so the term is
  // open()'d in the container BEFORE `proposeDimensions()` is called —
  // FitAddon's measurement needs the term parented, otherwise it falls back
  // to 80x24 and the agent's PTY is created at that size, wrapping its
  // first lines of output inside a wider pane (#302). `attach` is idempotent
  // with the sibling effect's call: when both fire on first mount, the
  // second one is a no-op (`attachToDOM` short-circuits on `inst.opened`).
  useEffect(() => {
    if (!node || !node.provider || node.status !== 'idle') return;
    if (!containerRef.current) return;
    const container = containerRef.current;
    let cancelled = false;
    (async () => {
      const inst = await terminalManager.attach(sessionId, container);
      if (cancelled || !inst) return;
      const dims = inst.fitAddon.proposeDimensions();
      try {
        await spawnAgent(sessionId, node.provider, dims?.rows, dims?.cols);
      } catch (e) {
        console.error('[AgentTerminal] Failed to auto-spawn agent:', e);
      }
    })();
    return () => { cancelled = true; };
  }, [sessionId, node?.provider, node?.status, spawnAgent]);

  // Resolve the default provider label for the handover menu item. The lookups
  // are memoised per process / per mesh (see handoverProviderCache), so the N
  // panes of a mesh share one get_default_provider + one list_providers call
  // instead of firing 2×N IPC calls on every mesh switch.
  useEffect(() => {
    if (!node) return;
    let cancelled = false;
    getHandoverProviderLabel(node.mesh_id).then(label => {
      if (!cancelled) setHandoverProviderLabel(label);
    });
    return () => { cancelled = true; };
  }, [node?.mesh_id]);

  return (
    <div
      ref={containerRef}
      data-node-id={sessionId}
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
      onWheel={handleWheel}
    >
      {isDragging && (
        <div className="absolute inset-0 bg-cyan-500/10 border-2 border-dashed border-cyan-500 rounded-lg flex items-center justify-center z-50 pointer-events-none">
          <span className="text-cyan-400 text-sm font-medium">Drop file to paste path</span>
        </div>
      )}

      {!atBottom && !isDragging && !searchOpen && (
        <button
          onClick={() => {
            instRef.current?.term.scrollToBottom();
            instRef.current?.term.focus();
          }}
          aria-label="Jump to latest output"
          data-testid="jump-to-bottom"
          className="absolute bottom-3 right-3 z-30 flex items-center gap-1 px-2.5 py-1 rounded-full bg-bg-card border border-border-default text-[11px] text-accent-cyan shadow-lg hover:bg-bg-base hover:border-accent-cyan transition-colors"
        >
          <span aria-hidden="true">↓</span> Latest
        </button>
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
          onContextMenu={(e) => { e.preventDefault(); e.stopPropagation(); }}
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
