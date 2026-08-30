import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { SerializeAddon } from '@xterm/addon-serialize';
import { SearchAddon } from '@xterm/addon-search';
import { WebLinksAddon } from '@xterm/addon-web-links';
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
import { terminalWebglPool } from './WebglRendererPool';
import { decodeBase64Bytes } from '../../lib/base64';
import { setTheme, type ThemeName } from '../../lib/theme';

export interface TerminalInstance {
  term: Terminal;
  fitAddon: FitAddon;
  serializeAddon: SerializeAddon;
  searchAddon: SearchAddon;
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

// Claude Code prints plain URLs, which WebLinksAddon recognises. Codex uses
// OSC 8 terminal hyperlinks; xterm routes those through `linkHandler`
// instead. Keep both paths on the opener plugin so the Tauri WebView never
// tries to navigate away from Buildmesh.
function openTerminalLink(_event: MouseEvent, uri: string): void {
  openUrl(uri).catch(console.error);
}

function measureAndFit(inst: TerminalInstance): void {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const charSizeService = (inst.term as any)['_core']?.['_charSizeService'];
  charSizeService?.measure();
  inst.fitAddon.fit();
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

    // GPU context budget: this terminal just became visible, so it earns a
    // WebGL renderer (LRU-capped pool; hidden panes fall back to DOM).
    terminalWebglPool.activate(`agent:${nodeId}`, inst.term);

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
    terminalWebglPool.release(`agent:${nodeId}`);
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
      api.unsubscribeAgentOutput(nodeId).catch(() => {});
      terminalWebglPool.release(`agent:${nodeId}`);
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
      const term = new Terminal({
        ...createTerminalOptions(),
        // xterm accepts HTTP(S) OSC 8 links by default. Do not opt in to
        // non-HTTP protocols here; terminal output is untrusted agent text.
        linkHandler: { activate: openTerminalLink },
      });

      const fitAddon = new FitAddon();
      term.loadAddon(fitAddon);
      const serializeAddon = new SerializeAddon();
      term.loadAddon(serializeAddon);
      const searchAddon = new SearchAddon();
      term.loadAddon(searchAddon);
      const webLinksAddon = new WebLinksAddon(openTerminalLink);
      term.loadAddon(webLinksAddon);
      // Align glyph widths with the Unicode 11+ tables that modern agent CLIs
      // (string-width) use to lay out tables/box-drawing. Without this, xterm
      // falls back to Unicode 6 widths and emoji rows shear their borders.
      // loadUnicode11Widths also patches the small set of BMP emoji the
      // upstream @xterm/addon-unicode11 ships with the wrong width (notably
      // ⚠ U+26A0) — see loadUnicode11Widths.ts for the rationale.
      loadUnicode11Widths(term);
      // Issue #1122: the WebGL renderer is attached LAZILY by
      // `WebglRendererPool.activate` on DOM attach, not here at creation.
      // Every WebGL renderer holds a live GPU context and Chromium caps
      // active contexts at ~16; with 15+ agent terminals created up front,
      // creation-time attachment constantly evicted older contexts (the
      // repeated "webgl context not restored" warnings) and churned the
      // GPU ahead of the driver resets that took the app down. The pool
      // keeps at most a few live contexts, pinned to the most recently
      // attached terminals; everything else uses xterm's DOM renderer —
      // see WebglRendererPool.ts and loadWebglRenderer.ts.

      const instance: TerminalInstance = {
        term,
        fitAddon,
        serializeAddon,
        searchAddon,
        unlisten: () => {},
        opened: false,
        resizeObserver: null,
        attachedContainer: null,
        onFindRequest: null,
      };

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

      // Binary PTY path (issue #1385). Production bytes arrive here as a
      // `Uint8Array` with no Base64/JSON. The `agent-output` listener
      // above stays for test injection (`line`) and the pre-subscribe
      // fallback. `TerminalWriter` still rAF-batches the display write.
      api.subscribeAgentOutput(nodeId, (bytes) => {
        this.writer.append(nodeId, bytes);
      }).catch(console.error);

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
