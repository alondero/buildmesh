import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { SerializeAddon } from '@xterm/addon-serialize';
import { SearchAddon } from '@xterm/addon-search';
import { WebLinksAddon } from '@xterm/addon-web-links';
import { WebglAddon } from '@xterm/addon-webgl';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import type { AgentOutputPayload } from '../../types/generated/AgentOutputPayload';
import type { AgentSpawnedPayload } from '../../types/generated/AgentSpawnedPayload';
import type { SerializeTerminalRequestPayload } from '../../types/generated/SerializeTerminalRequestPayload';

export type { AgentOutputPayload };
import { openUrl } from '@tauri-apps/plugin-opener';
import * as api from '../../lib/tauri';
import { createTerminalOptions } from './terminalConfig';
import { resolveKeyAction } from './terminalKeyAction';
import { isMac } from '../../lib/platform';
import { TerminalWriter, type TerminalWriteData } from './TerminalWriter';
import { FontSizeManager } from './FontSizeManager';
import { ThemeManager } from './ThemeManager';
import { loadUnicode11Widths } from './loadUnicode11Widths';
import { decodeBase64Bytes } from '../../lib/base64';
import { setTheme, type ThemeName } from '../../lib/theme';

export interface TerminalInstance {
  term: Terminal;
  fitAddon: FitAddon;
  serializeAddon: SerializeAddon;
  searchAddon: SearchAddon;
  webglHandle: WebglHandle;
  unlisten: UnlistenFn;
  opened: boolean;
  resizeObserver: ResizeObserver | null;
  attachedContainer: HTMLElement | null;
  onFindRequest: (() => void) | null;
}

function terminalDataFromPayload(payload: AgentOutputPayload): TerminalWriteData | null {
  // `!= null` catches both Rust's `None` (serialised as `null`) AND a
  // test-constructed literal that omits the field entirely (TypeScript
  // widens the missing key to `undefined` at runtime).
  if (payload.data != null) return decodeBase64Bytes(payload.data);
  if (payload.line != null) return payload.line;
  return null;
}

function measureAndFit(inst: TerminalInstance): void {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const charSizeService = (inst.term as any)['_core']?.['_charSizeService'];
  charSizeService?.measure();
  inst.fitAddon.fit();
}

/**
 * Result of attempting to attach a hardware-accelerated renderer to a
 * terminal. `webgl` carries the live addon + the context-loss subscriber
 * the registry needs to keep alive for the lifetime of the terminal; `dom`
 * means the addon constructor or `activate` rejected and xterm.js's default
 * DOM renderer stays in place (issue #1122 acceptance criterion: clean
 * fallback when WebGL is unavailable).
 */
type WebglHandle =
  | { kind: 'webgl'; addon: WebglAddon; contextLossListener: { dispose: () => void } }
  | { kind: 'dom' };

/**
 * Try to attach `@xterm/addon-webgl` to `term`. The terminal is rendered
 * by xterm.js's fallback DOM renderer until the addon activates, so a
 * failure here is invisible to the user — the only cost is staying on the
 * slower renderer. On a future context-loss event the addon is disposed
 * and re-attached once; if the re-attach also fails we log a warning and
 * stay on DOM for the lifetime of the terminal.
 *
 * The `setHandle` callback is the wire that lets a context-loss recovery
 * update the registry's stored handle. Without it the recovered addon's
 * `contextLossListener` would be unreferenced (the registry still holds
 * the disposed handle) and a second context-loss event would leak the
 * new addon's subscription — the registry's `dispose()` would also
 * operate on a torn-down handle. The callback runs synchronously inside
 * the context-loss handler, so the registry sees the swap before the
 * next paint.
 *
 * Issue #1122: the DOM renderer degrades from <2ms to 30-60ms+ per
 * keystroke once the scrollback spans thousands of lines, because TUI
 * cursor positioning forces style recalcs across the HTML <span> tree.
 * WebGL renders via a texture atlas and is constant-time per cell.
 */
function loadWebglRenderer(
  term: Terminal,
  nodeId: number,
  setHandle: (handle: WebglHandle) => void,
): WebglHandle {
  let addon: WebglAddon;
  try {
    addon = new WebglAddon();
  } catch (err) {
    // The constructor throws when WebGL2 is unsupported (Safari < 16, or
    // a host with WebGL disabled at the OS level). Stay on DOM — the
    // primary renderer is the fastest one we can get.
    console.warn(`[TerminalRegistry] WebGL unavailable for node ${nodeId}; falling back to DOM renderer`, err);
    return { kind: 'dom' };
  }

  try {
    term.loadAddon(addon);
  } catch (err) {
    // `loadAddon` defers to the addon's `activate`, which throws when it
    // can't build a WebGL2 context on the rendered canvas. Same outcome
    // as a constructor throw: stay on DOM.
    console.warn(`[TerminalRegistry] WebGL activate failed for node ${nodeId}; falling back to DOM renderer`, err);
    addon.dispose();
    return { kind: 'dom' };
  }

  // Context loss (driver crash, tab backgrounded past the GPU timeout,
  // user disables hardware acceleration) needs to fall back to DOM rather
  // than freeze the terminal. xterm.js disposes the active renderer when
  // the WebGL addon disposes itself, so the same path that originally
  // installed WebGL (above) re-installs it; if that retry also fails
  // (e.g. the driver really is gone) the terminal stays on DOM.
  let contextLossListener: { dispose: () => void } | null = null;
  try {
    contextLossListener = addon.onContextLoss(() => {
      console.warn(`[TerminalRegistry] WebGL context lost for node ${nodeId}; attempting re-attach`);
      // Dispose THIS handle's listener and addon so the closure is
      // released before the recursive call replaces the handle in the
      // registry. Without this, the old addon's `Emitter` subscription
      // (and the listener closure capturing it) would leak for the
      // lifetime of the registry.
      try {
        contextLossListener?.dispose();
      } catch {
        // Already disposed — safe to ignore.
      }
      try {
        addon.dispose();
      } catch {
        // Already disposed — safe to ignore.
      }
      const recovered = loadWebglRenderer(term, nodeId, setHandle);
      // Publish the swap to the registry so its stored handle matches
      // the live addon — `dispose()` will clean up the recovered
      // handle, not the torn-down one.
      setHandle(recovered);
      if (recovered.kind === 'dom') {
        console.warn(`[TerminalRegistry] WebGL re-attach failed for node ${nodeId}; staying on DOM renderer`);
      }
    });
  } catch (err) {
    // Older addons returned the event itself rather than a subscribable
    // function; if so, the listener wasn't registered and we'll get no
    // recovery — but the addon still works for the steady state.
    console.warn(`[TerminalRegistry] WebGL context-loss subscription unavailable for node ${nodeId}`, err);
  }

  return { kind: 'webgl', addon, contextLossListener: contextLossListener ?? { dispose: () => {} } };
}

export class TerminalRegistry {
  private instances = new Map<number, TerminalInstance>();
  private pending = new Map<number, Promise<TerminalInstance | null>>();
  private listeners = new Set<() => void>();
  private writer = new TerminalWriter();
  private fontSizeManager = new FontSizeManager();
  private themeManager = new ThemeManager();
  // unlisten handles for module-level listeners (e.g. the agent-spawned
  // reconcile), collected so destroy() can release them. The per-instance
  // `instance.unlisten` already covers the agent-output / serialize-request
  // listeners and is released by dispose().
  private unlistenFns: UnlistenFn[] = [];
  // Per-node `performance.now()` captured the first time `getOrCreate`
  // created a fresh instance for the node. Used as the elapsed reference
  // for the `xterm_mount` spawn-timing checkpoint (issue #602) so the
  // log line measures pure frontend mount latency — how long from
  // "frontend realized it needs an xterm" to "xterm is in the DOM" —
  // and stays self-contained (the frontend has no access to the Rust
  // `spawn_start` `Instant`). Cleared by `dispose` to avoid a slow leak
  // across many spawn/delete cycles.
  private nodeStartTimes = new Map<number, number>();

  constructor() {
    // Post-spawn PTY-size reconcile (issue #332). The backend emits
    // `agent-spawned` after an async-spawn path (auto-resume on startup,
    // fresh auto-spawn, handover, etc.) finishes registering the agent
    // in PROCESS_REGISTRY. By that point the agent is up and the
    // attach-fit's `resize_agent` IPC — which was swallowed earlier as
    // "Agent not running" — will now succeed, so we re-push the term's
    // real dimensions. `syncPtySize` is self-guarding: a no-op for
    // missing/detached instances, and it silently swallows the
    // "Agent not running" rejection in case of a race.
    this.registerListener<AgentSpawnedPayload>(
      'agent-spawned',
      (event) => {
        this.syncPtySize(event.payload.session_id);
      },
    );
  }

  private registerListener<T>(event: string, handler: (event: { payload: T }) => void): void {
    const unlistenPromise = listen<T>(event, handler);
    // Capture the unlisten function once listen() resolves. We don't
    // await here so the constructor stays synchronous — the unlisten
    // will be available by the time destroy() runs because the promise
    // resolves on the same microtask queue as the listener registration.
    unlistenPromise.then((unlisten) => {
      this.unlistenFns.push(unlisten);
    }).catch(console.error);
  }

  getInstance(nodeId: number): TerminalInstance | undefined {
    return this.instances.get(nodeId);
  }

  getTerminal(nodeId: number): Terminal | undefined {
    return this.instances.get(nodeId)?.term;
  }

  async getOrCreate(nodeId: number): Promise<TerminalInstance | null> {
    if (this.instances.has(nodeId)) {
      return this.instances.get(nodeId)!;
    }
    if (this.pending.has(nodeId)) {
      return this.pending.get(nodeId)!;
    }
    // Stamp the per-node spawn-timing reference NOW (issue #602). This is
    // the moment the frontend first sees a fresh nodeId — the "frontend
    // realized it needs an xterm" anchor that the `xterm_mount` checkpoint
    // at `attachToDOM` will measure against. Doing it here (rather than
    // inside `doCreate`) means concurrent callers of `getOrCreate` for the
    // same nodeId all share the original stamp: `pending` already
    // deduplicates them, and only the first path reaches this line.
    this.nodeStartTimes.set(nodeId, performance.now());
    const promise = this.doCreate(nodeId);
    this.pending.set(nodeId, promise);
    try {
      return await promise;
    } finally {
      this.pending.delete(nodeId);
    }
  }

  async attach(nodeId: number, container: HTMLElement): Promise<TerminalInstance | null> {
    const inst = await this.getOrCreate(nodeId);
    if (!inst) return null;
    return this.attachToDOM(nodeId, inst, container);
  }

  private attachToDOM(nodeId: number, inst: TerminalInstance, container: HTMLElement): TerminalInstance {
    const wasFreshOpen = !inst.opened;
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

    if (inst.resizeObserver) {
      inst.resizeObserver.disconnect();
    }
    inst.resizeObserver = new ResizeObserver(() => {
      requestAnimationFrame(() => {
        if (!inst.attachedContainer) return;
        measureAndFit(inst);
      });
    });
    inst.resizeObserver.observe(container);

    requestAnimationFrame(() => {
      measureAndFit(inst);
      // Only auto-scroll-to-tail on the first open. On re-attach the user
      // may have scrolled back to read history; forcing the tail here would
      // silently destroy that position (and flash the jump-to-latest pill).
      if (wasFreshOpen) inst.term.scrollToBottom();
      inst.term.refresh(0, inst.term.rows - 1);
    });

    // Issue #602: emit the `xterm_mount` spawn-timing checkpoint on fresh
    // open only. The line format mirrors the Rust `SpawnTimer` checkpoints
    // (`spawn_timing: session={} checkpoint={} elapsed={}ms`) so a reader
    // grepping `spawn_timing:` across `buildmesh.log` + the browser console
    // gets a single coherent timeline. Re-attach (fluid-grid pane swap,
    // workspace focus flip) re-parents an already-mounted xterm — it isn't
    // a mount, so it must NOT re-emit, or every pane swap would flood the
    // log. The elapsed reference is `getOrCreate`'s first-saw stamp: pure
    // frontend mount latency (no shared Instant with the Rust side).
    if (wasFreshOpen) {
      const start = this.nodeStartTimes.get(nodeId);
      if (start !== undefined) {
        console.info(
          `spawn_timing: session=${nodeId} checkpoint=xterm_mount elapsed=${Math.round(performance.now() - start)}ms`,
        );
      }
    }

    return inst;
  }

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

  fit(nodeId: number): void {
    const inst = this.instances.get(nodeId);
    if (!inst) return;
    measureAndFit(inst);
  }

  fitAll(): void {
    for (const [nodeId, inst] of this.instances) {
      if (inst.attachedContainer) {
        this.fit(nodeId);
      }
    }
  }

  /**
   * Reconcile the PTY size to the terminal's *current* dimensions. Call this
   * right after an agent spawns.
   *
   * The attach-time fit runs before the agent process exists, so the
   * `resize_agent` it triggers via `term.onResize` is rejected as "Agent not
   * running" and swallowed — and spawn itself falls back to 80x24 when
   * `proposeDimensions()` returns undefined (term not yet laid out). Because
   * `term.cols` is by then already the fitted value, no further `onResize`
   * fires, so nothing re-pushes the real size. The agent ends up wrapping its
   * output and input at 80 cols inside a much wider pane. This unconditional
   * re-send closes that gap once the agent is definitely up.
   *
   * The constructor's `agent-spawned` listener (issue #332) is the canonical
   * caller, covering all async-spawn paths uniformly: auto-resume on startup,
   * fresh auto-spawn, handover. The method stays public for the same reason
   * the regression test in terminal-registry.test.ts pins it: the contract
   * ("re-push the term's real dimensions to the PTY once the agent is up")
   * is load-bearing and worth a named entry point.
   */
  syncPtySize(nodeId: number): void {
    const inst = this.instances.get(nodeId);
    if (!inst || !inst.attachedContainer) return;
    measureAndFit(inst);
    const { cols, rows } = inst.term;
    api.resizeAgent(nodeId, rows, cols).catch((err) => {
      if (err !== 'Agent not running') {
        console.error(`[TerminalRegistry] PTY size sync failed for node ${nodeId}:`, err);
      }
    });
  }

  dispose(nodeId: number): void {
    const instance = this.instances.get(nodeId);
    if (instance) {
      if (instance.resizeObserver) {
        instance.resizeObserver.disconnect();
      }
      instance.unlisten();
      // Dispose the WebGL addon BEFORE the terminal. The addon's disposer
      // restores xterm's default DOM renderer; calling it after
      // `instance.term.dispose()` would be a no-op against a torn-down
      // terminal and the addon would leak its `Disposable` subscriptions.
      if (instance.webglHandle.kind === 'webgl') {
        instance.webglHandle.contextLossListener.dispose();
        try {
          instance.webglHandle.addon.dispose();
        } catch {
          // Already disposed by a context-loss path — safe to ignore.
        }
      }
      instance.term.dispose(); // allow-dispose — keyed by deleted-node IPC, the registry's only legit dispose path
      this.instances.delete(nodeId);
      this.writer.unregister(nodeId);
      this.fontSizeManager.unregister(nodeId);
      this.themeManager.unregister(nodeId);
      // Drop the per-node spawn-timing stamp (issue #602). Without this
      // every spawn/delete cycle adds an entry to `nodeStartTimes` and
      // the map grows unbounded for long-running sessions that cycle
      // through many nodes.
      this.nodeStartTimes.delete(nodeId);
      this.notify();
    }
  }

  /**
   * Push the named theme to every live terminal AND the <html data-theme>
   * attribute (so the CSS cascade picks up the new token values for the
   * rest of the app). Idempotent — re-applying the active value re-syncs
   * the DOM (in case something external mutated it) but only fans out
   * listeners on a real flip.
   *
   * Issue #734: this is the registry's public entry point for code
   * (mainly tests) that wants to flip the theme without reaching into
   * theme.ts directly. The settings modal just calls `setTheme(...)` —
   * both registries' ThemeManager instances subscribe to the module-
   * level pub/sub at construction, so one `setTheme` call updates both
   * xterm maps AND the DOM in lockstep.
   */
  applyTheme(theme: ThemeName): void {
    setTheme(theme);
  }

  subscribe(cb: () => void): () => void {
    this.listeners.add(cb);
    return () => { this.listeners.delete(cb); };
  }

  private notify(): void {
    this.listeners.forEach(cb => cb());
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
      // Align glyph widths with the Unicode 11+ tables that modern agent CLIs
      // (string-width) use to lay out tables/box-drawing. Without this, xterm
      // falls back to Unicode 6 widths and emoji rows shear their borders.
      // loadUnicode11Widths also patches the small set of BMP emoji the
      // upstream @xterm/addon-unicode11 ships with the wrong width (notably
      // ⚠ U+26A0) — see loadUnicode11Widths.ts for the rationale.
      loadUnicode11Widths(term);

      // Hardware-accelerated rendering. Must happen AFTER every other addon
      // is loaded but BEFORE `term.open` (in attachToDOM) so the WebGL
      // renderer is the one that paints the first frame. The helper stays
      // on xterm.js's DOM renderer if WebGL2 is unavailable or activation
      // throws — see loadWebglRenderer for the fallback contract.
      //
      // The setter closure lets a context-loss recovery swap the stored
      // handle in place; the instance object is constructed *before* the
      // call so the closure captures `instance` by reference (not by
      // value) and updates its `webglHandle` field in place when a
      // recovery fires. This is the only way to keep the registry's
      // stored handle in sync with the live addon across multiple GPU
      // context losses — a `const` field would freeze the registry on
      // the original (now disposed) handle.
      const instance: TerminalInstance = {
        term,
        fitAddon,
        serializeAddon,
        searchAddon,
        // Placeholder — overwritten below by the WebGL loader, and
        // potentially rewritten again by a context-loss recovery.
        webglHandle: { kind: 'dom' },
        unlisten: () => {},
        opened: false,
        resizeObserver: null,
        attachedContainer: null,
        onFindRequest: null,
      };
      instance.webglHandle = loadWebglRenderer(term, nodeId, (fresh) => {
        instance.webglHandle = fresh;
      });

      this.writer.register(nodeId, (data) => term.write(data));
      this.fontSizeManager.register(nodeId, term, () => measureAndFit(instance));
      // Issue #734: register the new term with the ThemeManager so a later
      // theme flip (handled by ThemeManager's onTerminalThemeChange listener)
      // pushes the matching xterm.js palette into term.options.theme.
      this.themeManager.register(nodeId, term);

      const unlisten = await listen<AgentOutputPayload>('agent-output', (event) => {
        if (event.payload.session_id === nodeId) {
          const data = terminalDataFromPayload(event.payload);
          if (data !== null) {
            this.writer.append(nodeId, data);
          }
        }
      });

      const unlistenSerialize = await listen<SerializeTerminalRequestPayload>('serialize-terminal-request', (event) => {
        if (event.payload.node_id === nodeId) {
          const snapshot = instance.serializeAddon.serialize({ scrollback: 200 });
          api.submitTerminalSnapshot(event.payload.request_id, snapshot).catch(console.error);
        }
      });

      term.onData((data) => {
        api.writeToAgent(nodeId, data).catch(console.error);
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

        switch (action) {
          case 'copy':
            navigator.clipboard.writeText(term.getSelection()).catch(err => {
              console.warn('[TerminalRegistry] Clipboard write failed:', err);
            });
            return false;
          case 'paste':
            ev.preventDefault();
            api.readClipboard().then(text => {
              if (text) term.paste(text);
            }).catch(() => {
              navigator.clipboard.readText().then(text => {
                if (text) term.paste(text);
              }).catch(err => {
                console.warn('[TerminalRegistry] Clipboard read failed:', err);
              });
            });
            return false;
          case 'selectAll':
            term.selectAll();
            return false;
          case 'find':
            instance.onFindRequest?.();
            return false;
          case 'clear':
            term.clear();
            return false;
          case 'passthrough':
            return true;
        }
      });

      term.onResize(({ cols, rows }) => {
        api.resizeAgent(nodeId, rows, cols).catch(err => {
          if (err !== 'Agent not running') {
            console.error(`[TerminalRegistry] Resize failed for node ${nodeId}:`, err);
          }
        });
      });

      const combinedUnlisten: UnlistenFn = () => { unlisten(); unlistenSerialize(); };
      instance.unlisten = combinedUnlisten;

      this.instances.set(nodeId, instance);
      this.notify();
      return instance;
    } catch (e) {
      console.error(`[TerminalRegistry] Failed to create terminal for ${nodeId}`, e);
      // Drop the spawn-timing stamp set by `getOrCreate` (issue #602). The
      // success path's `dispose(nodeId)` is the normal cleanup, but it
      // walks `this.instances` and never runs when creation failed before
      // the instance was inserted — leaving a dead entry in the map.
      // Without this, a long-running session that fails many creates
      // (e.g. a corrupted `@xterm/xterm` import) would leak the map
      // unbounded alongside the console errors above.
      this.nodeStartTimes.delete(nodeId);
      return null;
    }
  }

  destroy(): void {
    for (const nodeId of this.instances.keys()) {
      this.dispose(nodeId); // allow-dispose — destroy() only runs at app-exit / test teardown
    }
    this.fontSizeManager.destroy();
    this.themeManager.destroy();
    for (const unlisten of this.unlistenFns) {
      unlisten();
    }
    this.unlistenFns = [];
  }
}
