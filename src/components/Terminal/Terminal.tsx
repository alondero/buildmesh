import { useEffect, useRef, useState, useCallback, type WheelEvent as ReactWheelEvent } from 'react';
import { createPortal } from 'react-dom';
import '@xterm/xterm/css/xterm.css';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useUIStore } from '../../stores/uiStore';
import * as api from '../../lib/tauri';
import { terminalFontSize, setTerminalFontSize, TERMINAL_FONT_SIZE_DEFAULT, SEARCH_DECORATIONS } from './terminalConfig';
import { resolveZoomKeyAction } from './terminalKeyAction';
import { isMac } from '../../lib/platform';
import { TerminalRegistry, type TerminalInstance } from './TerminalRegistry';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { useClickOutside } from '../../hooks/useClickOutside';
import { dropdownId } from '../../lib/dropdownId';

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

// Module-level guard so the window keydown listener is installed
// exactly once across the lifetime of the app — see `installTerminalZoomListener`
// below. Tests that swap `window.setTimeout` etc. still see this fire
// once at first mount.
let terminalZoomListenerInstalled = false;

/**
 * Idempotently installs the window-level Ctrl/Cmd+0/+/- zoom listener
 * (issue #1264). Mirrors the `installListener()` pattern in
 * `src/lib/pathInvalidatedCache.ts:337` — the first `AgentTerminal` to
 * mount registers the listener, every subsequent mount early-returns
 * because the global is already `true`. We never uninstall because:
 *
 *   1. The handler is a pure read of `terminalFontSize()` + a write
 *      to the same module variable; it's safe to leave armed even
 *      when no terminal pane is mounted (keystrokes are silently
 *      dropped because `setTerminalFontSize` is a setter on a module
 *      variable that nothing else reads).
 *   2. The `AgentTerminal` component is the only consumer and it
 *      mounts/unmounts frequently in grid reshuffles — installing
 *      per-mount would register N listeners for N panes and add
 *      N× window teardown cost.
 */
function installTerminalZoomListener(): void {
  if (terminalZoomListenerInstalled) return;
  terminalZoomListenerInstalled = true;
  const handler = (e: KeyboardEvent) => {
    // Cheap out for the overwhelming majority of keystrokes that don't carry
    // a modifier at all — keeps this window-level listener off the hot path
    // for plain text input.
    if (!e.ctrlKey && !e.metaKey) return;
    const action = resolveZoomKeyAction({
      key: e.key,
      ctrlKey: e.ctrlKey,
      shiftKey: e.shiftKey,
      metaKey: e.metaKey,
      isMac,
    });
    if (action === 'reset') {
      e.preventDefault();
      setTerminalFontSize(TERMINAL_FONT_SIZE_DEFAULT);
    } else if (action === 'in') {
      e.preventDefault();
      setTerminalFontSize(terminalFontSize() + 2);
    } else if (action === 'out') {
      e.preventDefault();
      setTerminalFontSize(terminalFontSize() - 2);
    }
  };
  window.addEventListener('keydown', handler);
}

/** Test-only: reset the singleton guard so each test can re-install
 * the listener fresh. Mirrors `resetPathInvalidatedCacheForTests` in
 * `pathInvalidatedCache.ts:371`. */
export function resetTerminalZoomListenerForTests(): void {
  terminalZoomListenerInstalled = false;
}

export function AgentTerminal({ nodeId, focusOnAttach = true }: { nodeId: number; focusOnAttach?: boolean }) {
  const focusOnAttachRef = useRef(focusOnAttach);
  focusOnAttachRef.current = focusOnAttach;
  const containerRef = useRef<HTMLDivElement>(null);
  const instRef = useRef<TerminalInstance | null>(null);
  const scrollDisposableRef = useRef<{ dispose: () => void } | null>(null);
  const isDragging = useUIStore(state => state.dragTargetNodeId === nodeId);
  const [contextMenu, setContextMenu] = useState<{ x: number; y: number } | null>(null);
  const [searchOpen, setSearchOpen] = useState(false);
  const [searchQuery, setSearchQuery] = useState('');
  const [atBottom, setAtBottom] = useState(true);
  const searchInputRef = useRef<HTMLInputElement>(null);
  const [handoverProviderLabel, setHandoverProviderLabel] = useState<string | null>(null);
  const spawnAgent = useAgentNodeStore(state => state.spawnAgent);
  // Subscribe to *this* node only via the normalized `nodesById` map (issue
  // #1384). The store reconciles on every fetch, preserving the same object
  // reference for unchanged rows — so this selector only triggers a re-render
  // when THIS specific node changes (status flip, rename, etc.). Other
  // nodes' attention events no longer cascade into this pane.
  const node = useAgentNodeStore(state => state.nodesById[nodeId]);

  // OS file drops (Explorer / Finder) are handled window-level in
  // useFileDropToTerminal — Tauri intercepts the native drop before the DOM, so
  // an element-level HTML5 onDrop never receives the files.

  const handleWheel = useCallback((e: ReactWheelEvent<HTMLDivElement>) => {
    // ⌘ on macOS, Ctrl elsewhere — same platform split as the keyboard zoom
    // handler above (issue #667).
    const mod = isMac ? e.metaKey : e.ctrlKey;
    if (mod) {
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
    const inst = terminalManager.getInstance(nodeId);
    if (inst && inst.term.hasSelection()) {
      navigator.clipboard.writeText(inst.term.getSelection()).catch(console.error);
    }
    setContextMenu(null);
  };

  const handlePaste = () => {
    const inst = terminalManager.getInstance(nodeId);
    if (inst) {
      // readClipboard uses pbpaste on macOS to bypass the WKWebView
      // clipboard-permission popup (macOS 14+). On other platforms it errors and
      // we fall back to the web clipboard API.
      api.readClipboard().then(text => {
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
    const inst = terminalManager.getInstance(nodeId);
    if (inst) inst.term.selectAll();
    setContextMenu(null);
  };

  const handleFind = () => {
    setContextMenu(null);
    setSearchOpen(true);
  };

  const handleClear = () => {
    const inst = terminalManager.getInstance(nodeId);
    if (inst) inst.term.clear();
    setContextMenu(null);
  };

  const handleHandover = async () => {
    const inst = terminalManager.getInstance(nodeId);
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

  // Dismiss context menu — issue #814 converged on the shared
  // `useClickOutside` hook (#492) for the outside-mousedown path.
  // `nodeId` scopes the selector (`[data-dropdown-for="<nodeId>"]`)
  // so two terminals with open context menus wouldn't interfere. The
  // Escape handler is separate — `useClickOutside` doesn't cover keys.
  //
  // Issue #1264 — prefix with the surface tag so a terminal-keyed
  // context menu can't collide with a mesh- or node-keyed menu that
  // shares the same numeric id (mesh and node ids both autoincrement
  // from the same SQLite sequence, so collisions are routine).
  useClickOutside<string>(contextMenu ? dropdownId('terminal', nodeId) : null, () => setContextMenu(null));
  useEffect(() => {
    if (!contextMenu) return;
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') setContextMenu(null);
    };
    document.addEventListener('keydown', handleKeyDown);
    return () => document.removeEventListener('keydown', handleKeyDown);
  }, [contextMenu]);

  // Focus search input when opened
  useEffect(() => {
    if (searchOpen) {
      requestAnimationFrame(() => searchInputRef.current?.focus());
    } else {
      setSearchQuery('');
      const inst = terminalManager.getInstance(nodeId);
      if (inst) inst.searchAddon.clearDecorations();
    }
  }, [searchOpen, nodeId]);

  const handleSearchChange = useCallback((e: React.ChangeEvent<HTMLInputElement>) => {
    const query = e.target.value;
    setSearchQuery(query);
    const inst = terminalManager.getInstance(nodeId);
    if (!inst) return;
    if (query) {
      inst.searchAddon.findNext(query, { incremental: true, decorations: SEARCH_DECORATIONS });
    } else {
      inst.searchAddon.clearDecorations();
    }
  }, [nodeId]);

  const handleSearchNext = useCallback(() => {
    const inst = terminalManager.getInstance(nodeId);
    if (inst && searchQuery) {
      inst.searchAddon.findNext(searchQuery, { decorations: SEARCH_DECORATIONS });
    }
  }, [nodeId, searchQuery]);

  const handleSearchPrev = useCallback(() => {
    const inst = terminalManager.getInstance(nodeId);
    if (inst && searchQuery) {
      inst.searchAddon.findPrevious(searchQuery, { decorations: SEARCH_DECORATIONS });
    }
  }, [nodeId, searchQuery]);

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
  // (Cmd on macOS — see resolveZoomKeyAction in terminalKeyAction.ts.)
  //
  // Issue #1264 — N terminal panes previously registered N identical
  // window listeners (the handler was inside a `useEffect(() => ...)`
  // with `[]` deps that ran on every mount). The behaviour is
  // naturally idempotent (`setTerminalFontSize(x)` is a no-op for an
  // unchanged value), so the duplicate listeners only cost N× window
  // teardown on every grid reshuffle — wasteful, not broken. Hoist to
  // a module-level singleton installed once, mirroring the
  // `installListener()` pattern in `pathInvalidatedCache.ts:337`.
  useEffect(() => {
    installTerminalZoomListener();
  }, []);

  // Focus guardian. In a multi-pane grid, background DOM churn — a re-render
  // that momentarily re-parents xterm's imperatively-appended `.xterm` element,
  // an overlay mounting/unmounting next to it, or a WebView2 focus hiccup — can
  // blur the terminal's hidden helper-textarea and drop keyboard focus to
  // <body> mid-keystroke, with no user action, so the next characters go
  // nowhere. When *this* node is the active one and the app window still holds
  // OS focus, reclaim it. We act ONLY when focus has fallen to nothing
  // (<body>/null): a click that moves focus to a real control (the search box, a
  // rename input, a header button, another pane, or — the case that motivated
  // this comment — a textarea in the dock like the Scratch Pad or Mesh
  // Properties tab) lands on that element, so the guard skips and never
  // fights a legitimate move.
  //
  // The primary check uses `focusout.relatedTarget` (where focus is *going*),
  // not `document.activeElement` at microtask time. The microtask check is
  // still there as a belt-and-braces fallback for when relatedTarget is null
  // (programmatic focus changes leave it null; focus leaving the document
  // also yields null), but trusting relatedTarget first closes the
  // Chromium/WebView2 race where the focus event on the new control hasn't
  // committed to activeElement by the time the microtask runs — without
  // the relatedTarget short-circuit, the guard would see <body> and yank
  // focus back from the user's click. activeNodeId is read via getState()
  // so the listener doesn't need re-binding on every switch.
  //
  // Note we intentionally do NOT also reclaim when activeElement is
  // `<html>`: Tab-navigation out of the WebView can briefly surface the
  // document root as activeElement, and reclaiming there would pull focus
  // back from the user's deliberate Tab-out.
  useEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const onFocusOut = (e: FocusEvent) => {
      // `relatedTarget` is the element receiving focus, or null when
      // focus is leaving the document / going to body. If it's a real
      // focusable control (input, textarea, select, button,
      // [contenteditable], etc.) the user is moving focus there — never
      // reclaim against that.
      const rt = e.relatedTarget as HTMLElement | null;
      if (rt && rt !== document.body) return;
      // Defer a microtask so document.activeElement reflects where focus
      // actually landed before we judge whether it fell out entirely. The
      // relatedTarget check above already covers the common case (click
      // onto a focusable control); this catches the remaining <body>/null
      // edge cases — chiefly a stray DOM reconciliation that drops focus
      // with no element waiting to receive it.
      queueMicrotask(() => {
        if (nodeId !== useAgentNodeStore.getState().activeNodeId) return;
        if (typeof document.hasFocus === 'function' && !document.hasFocus()) return;
        const active = document.activeElement;
        if (active && active !== document.body) return; // moved to a real control — leave it
        // Read the live instance from the registry (not instRef): a node closed
        // mid-flight is removed from the registry, so this skips it rather than
        // focusing a disposed xterm (dispose() doesn't null attachedContainer).
        const inst = terminalManager.getInstance(nodeId);
        if (!inst || !inst.attachedContainer) return;
        console.warn(`[AgentTerminal] keyboard focus fell to <body> while node ${nodeId} was active — restoring terminal focus`);
        inst.term.focus();
      });
    };
    container.addEventListener('focusout', onFocusOut);
    return () => container.removeEventListener('focusout', onFocusOut);
  }, [nodeId]);

  // Attach/detach the xterm element. Keyed on nodeId ONLY. It used to also
  // depend on node.status, which meant every attention flip (running ↔
  // awaiting_input, many per minute while an agent works) tore the terminal
  // element out of the DOM and re-appended it — a visible reflow and a
  // micro-hang. The status-driven auto-spawn lives in its own effect below so
  // it no longer drags the DOM lifecycle with it.
  //
  // The xterm itself is owned by `TerminalRegistry` — `attach`/`detach`
  // reference-count it. The cleanup here releases the things this effect
  // *adds on top of* the registry's term: the scroll listener, the
  // `onFindRequest` callback, and the registry ref. The helper's returned
  // cleanup runs after the signal abort, so any pending `.then` callback
  // for `attach` short-circuits on `signal.aborted` first.
  useAsyncEffect((signal) => {
    if (!containerRef.current) return;
    const container = containerRef.current;
    setAtBottom(true);

    terminalManager.attach(nodeId, container).then((inst) => {
      if (signal.aborted || !inst) return;
      instRef.current = inst;
      inst.onFindRequest = () => setSearchOpen(true);

      const updateAtBottom = () => {
        const buf = inst.term.buffer.active;
        setAtBottom(buf.viewportY >= buf.baseY);
      };
      scrollDisposableRef.current?.dispose(); // allow-dispose — scroll listener, not the xterm terminal
      scrollDisposableRef.current = inst.term.onScroll(updateAtBottom);
      updateAtBottom();

      // Initial activation focuses the terminal. Subsequent tab selection is
      // delegated by NodeCard, so keyboard navigation keeps focus on the tab.
      if (nodeId === useAgentNodeStore.getState().activeNodeId && focusOnAttachRef.current) {
        inst.term.focus();
      }
    });

    return () => {
      const inst = terminalManager.getInstance(nodeId);
      if (inst) inst.onFindRequest = null;
      scrollDisposableRef.current?.dispose(); // allow-dispose — scroll listener, not the xterm terminal
      scrollDisposableRef.current = null;
      terminalManager.detach(nodeId);
    };
  // Focus intent and the active node are read after async attachment; keyboard
  // tab selection must not lose focus when the terminal finishes mounting.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [nodeId]);

  // Auto-spawn the agent for a freshly created (idle) node. Separated from the
  // attach effect so it can react to the status becoming 'idle' without
  // re-attaching the DOM. We use `attach` (not `getOrCreate`) so the term is
  // open()'d in the container BEFORE `proposeDimensions()` is called —
  // FitAddon's measurement needs the term parented, otherwise it falls back
  // to 80x24 and the agent's PTY is created at that size, wrapping its
  // first lines of output inside a wider pane (#302). `attach` is idempotent
  // with the sibling effect's call: when both fire on first mount, the
  // second one is a no-op (`attachToDOM` short-circuits on `inst.opened`).
  //
  // The xterm itself is owned by `TerminalRegistry`; this effect only
  // needs to drop its setState on cleanup (no resource teardown). The
  // helper's signal-abort covers it.
  useAsyncEffect((signal) => {
    if (!node || !node.provider || node.status !== 'idle') return;
    if (!containerRef.current) return;
    const container = containerRef.current;
    (async () => {
      const inst = await terminalManager.attach(nodeId, container);
      if (signal.aborted || !inst) return;
      const dims = inst.fitAddon.proposeDimensions();
      try {
        await spawnAgent(nodeId, node.provider, dims?.rows, dims?.cols);
      } catch (e) {
        console.error('[AgentTerminal] Failed to auto-spawn agent:', e);
      }
    })();
  }, [nodeId, node?.provider, node?.status, spawnAgent]);

  // Resolve the default provider label for the handover menu item. The
  // `api.getDefaultProvider` and `api.listProviders` wrappers are memoised
  // in `src/lib/tauri.ts` (issue #405) — per mesh / per process, with
  // rejection-evict — so the N panes of a mesh share one
  // `get_default_provider` + one `list_providers` call instead of firing
  // 2×N IPC calls on every mesh switch.
  useAsyncEffect((signal) => {
    if (!node) return;
    (async () => {
      try {
        const [defProvider, providers] = await Promise.all([
          api.getDefaultProvider(node.mesh_id),
          api.listProviders(),
        ]);
        if (signal.aborted) return;
        const match = providers.find(p => p.id === defProvider);
        setHandoverProviderLabel(match?.label ?? defProvider);
      } catch {
        if (signal.aborted) return;
        setHandoverProviderLabel('Default');
      }
    })();
  }, [node?.mesh_id]);

  return (
    <div
      ref={containerRef}
      data-node-id={nodeId}
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
        <div className="absolute inset-0 bg-accent-cyan/10 border-2 border-dashed border-accent-cyan rounded-lg flex items-center justify-center z-50 pointer-events-none">
          <span className="text-accent-cyan text-sm font-medium">Drop file to paste path</span>
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
          className="absolute bottom-3 right-3 z-30 flex items-center gap-1 px-2.5 py-1 rounded-full bg-bg-card border border-border-default text-xs text-accent-cyan shadow-lg hover:bg-bg-base hover:border-accent-cyan transition-colors"
        >
          <span aria-hidden="true">↓</span> Latest
        </button>
      )}

      {searchOpen && (
        <div className="absolute top-1 right-1 z-50 flex items-center gap-1 bg-bg-card border border-border-default rounded-md px-2 py-1 shadow-md animate-fade-in">
          <input
            ref={searchInputRef}
            type="text"
            value={searchQuery}
            onChange={handleSearchChange}
            onKeyDown={handleSearchKeyDown}
            placeholder="Find..."
            className="bg-transparent text-xs text-text-primary outline-none w-36 placeholder:text-text-muted"
          />
          <button type="button" onClick={handleSearchPrev} className="text-text-muted hover:text-accent-cyan text-xs px-1 transition-colors" title="Previous (Shift+Enter)" aria-label="Previous match">&#9650;</button>
          <button type="button" onClick={handleSearchNext} className="text-text-muted hover:text-accent-cyan text-xs px-1 transition-colors" title="Next (Enter)" aria-label="Next match">&#9660;</button>
          <button type="button" onClick={() => { setSearchOpen(false); instRef.current?.term.focus(); }} className="text-text-muted hover:text-status-error text-xs px-1 transition-colors" title="Close (Esc)" aria-label="Close search">&#10005;</button>
        </div>
      )}

      {/* Issue #1291 — portaled to `document.body` so `position:fixed`
          stays in viewport coordinates. The terminal pane nests inside
          the GridNodeHeader / GridSplitter hierarchy, and xterm's own
          container establishes a containing block (a few xterm internals
          apply `transform`/`will-change` for the renderer). Without the
          portal, `top`/`left` in viewport pixels anchored to the wrong
          box and the menu drifted on grid resize / split transitions. */}
      {contextMenu && createPortal(
        <div
          data-dropdown-for={dropdownId('terminal', nodeId)}
          className="fixed bg-bg-card border border-border-default rounded-md shadow-md z-[100] py-1 min-w-[160px] animate-scale-in origin-top-left"
          style={{ top: contextMenu.y, left: contextMenu.x }}
          onMouseDown={(e) => e.stopPropagation()}
          onContextMenu={(e) => { e.preventDefault(); e.stopPropagation(); }}
        >
          <button
            onClick={handleCopy}
            disabled={!instRef.current?.term.hasSelection()}
            className="w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors disabled:text-text-muted disabled:cursor-default disabled:hover:bg-transparent"
          >
            Copy <span className="float-right text-text-muted">{isMac ? '⌘C' : 'Ctrl+Shift+C'}</span>
          </button>
          <button
            onClick={handlePaste}
            className="w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            Paste <span className="float-right text-text-muted">{isMac ? '⌘V' : 'Ctrl+Shift+V'}</span>
          </button>
          <div className="border-t border-border-default my-0.5" />
          <button
            onClick={handleSelectAll}
            className="w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            Select All <span className="float-right text-text-muted">{isMac ? '⌘A' : 'Ctrl+Shift+A'}</span>
          </button>
          <button
            onClick={handleFind}
            className="w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            Find... <span className="float-right text-text-muted">{isMac ? '⌘F' : 'Ctrl+Shift+F'}</span>
          </button>
          {handoverProviderLabel !== null && (
            <>
              <div className="border-t border-border-default my-0.5" />
              <button
                onClick={handleHandover}
                disabled={!instRef.current?.term.hasSelection()}
                className="w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors disabled:text-text-muted disabled:cursor-default disabled:hover:bg-transparent"
              >
                Handover to new Node [{handoverProviderLabel}]
              </button>
            </>
          )}
          <div className="border-t border-border-default my-0.5" />
          <button
            onClick={handleClear}
            className="w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-base hover:text-accent-cyan transition-colors"
          >
            Clear Terminal <span className="float-right text-text-muted">{isMac ? '⌘+Shift+K' : 'Ctrl+Shift+L'}</span>
          </button>
        </div>,
        document.body,
      )}
    </div>
  );
}
