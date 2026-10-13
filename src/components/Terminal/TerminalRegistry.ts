// Issue #1568 - xterm.js and its addons are the heaviest single dependency
// in the desktop bundle (~250 KB minified, ~80 KB gzip). The TerminalRegistry
// class is a singleton whose `doCreate` only runs when an actual xterm is
// first attached, so we keep the runtime imports (Terminal, FitAddon,
// SerializeAddon, SearchAddon, WebLinksAddon, loadUnicode11Widths) lazy
// behind dynamic `import()` inside `doCreate` and convert the type imports
// to `import type` so Rollup erases them entirely. The class itself, plus
// the writers/managers it composes, are still cheap at module load.
import type { Terminal } from '@xterm/xterm';
import type { FitAddon } from '@xterm/addon-fit';
import type { SerializeAddon } from '@xterm/addon-serialize';
import type { SearchAddon } from '@xterm/addon-search';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import type { AgentOutputPayload } from '../../types/generated/AgentOutputPayload';
import type { AgentSpawnedPayload } from '../../types/generated/AgentSpawnedPayload';
import type { SerializeTerminalRequestPayload } from '../../types/generated/SerializeTerminalRequestPayload';

export type { AgentOutputPayload };
import * as api from '../../lib/tauri';
import { createTerminalOptions } from './terminalConfig';
import { resolveKeyAction } from './terminalKeyAction';
import { isMac } from '../../lib/platform';
import { TerminalWriter, type TerminalWriteData } from './TerminalWriter';
import type { TerminalLinkRouter } from './TerminalLinkRouter';
import { FontSizeManager } from './FontSizeManager';
import { ThemeManager } from './ThemeManager';
import { terminalWebglPool } from './WebglRendererPool';
import { decodeBase64Bytes } from '../../lib/base64';
import { setTheme, type ThemeName } from '../../lib/theme';
import { TerminalResizeScheduler } from './TerminalResizeScheduler';

export interface TerminalInstance {
  term: Terminal;
  fitAddon: FitAddon;
  serializeAddon: SerializeAddon;
  searchAddon: SearchAddon;
  links: TerminalLinkRouter;
  unlisten: UnlistenFn;
  opened: boolean;
  resizeScheduler: TerminalResizeScheduler;
  attachedContainer: HTMLElement | null;
  onFindRequest: (() => void) | null;
  useNativeClipboardPaste: boolean;
}

function terminalDataFromPayload(payload: AgentOutputPayload): TerminalWriteData | null {
  // `!= null` catches both Rust's `None` (serialised as `null`) AND a
  // test-constructed literal that omits the field entirely (TypeScript
  // widens the missing key to `undefined` at runtime).
  if (payload.data != null) return decodeBase64Bytes(payload.data);
  if (payload.line != null) return payload.line;
  return null;
}

// Link clicks — OSC 8 hyperlinks and plain printed URLs alike — are routed by
// `TerminalLinkRouter`, which owns the click arbitration. See its header.

/**
 * Bookkeeping for one in-flight `doCreate` (issue #2016).
 *
 * Creation spans several awaits — the lazy xterm chunk import, two
 * `listen()` registrations and the binary PTY Channel — and a node can be
 * deleted at any point inside that window. `dispose(nodeId)` used to clean
 * up only an instance already in `instances`, so a late `doCreate` went on
 * to register a terminal, writer sink, font-size / theme entries and Tauri
 * listeners for a node that no longer existed, and a failure after those
 * registrations kept them all.
 *
 * The record owns each resource as the creation acquires it, so a deletion
 * releases what is already held and refuses everything after it. It is
 * created by `getOrCreate` and dropped as soon as the creation settles, so
 * it is deliberately NOT a deleted-id tombstone map — that would only trade
 * one lifetime leak for another.
 */
class PendingCreation {
  private owned: Array<() => void> = [];
  /** True once ownership has moved on (committed or released). */
  private closed = false;

  /** The guarded creation. Assigned by `getOrCreate` right after it is
   *  constructed, so callers that coalesce onto an in-flight record can
   *  await it. */
  promise!: Promise<TerminalInstance | null>;

  /** Flipped by `dispose` and `destroy`. */
  cancelRequested = false;

  /**
   * Take ownership of one just-acquired resource. If the creation was
   * already closed — a deletion raced this line — the resource is released
   * on the spot, so callers never have to re-check `cancelRequested`.
   */
  own(release: () => void): void {
    if (this.closed) {
      release();
      return;
    }
    this.owned.push(release);
  }

  /**
   * Hand the acquired resources over to the `instances` map, which
   * `dispose(nodeId)` already tears down. Only the published success path
   * calls this; after it, any later `own()` releases immediately rather
   * than parking a resource nothing will ever free.
   */
  commit(): void {
    this.owned.length = 0;
    this.closed = true;
  }

  /**
   * Release everything acquired so far, newest first so the xterm outlives
   * the bookkeeping that points at it. Idempotent and safe to call from
   * `dispose`, `destroy`, the cancellation checkpoints and the failure path.
   *
   * Every disposer runs even if an earlier one throws: these are all
   * best-effort teardown steps (the unsubscribe is already a `.catch(() =>
   * {})`), and letting one failure skip the rest would leak exactly the
   * resources this record exists to release.
   */
  release(): void {
    if (this.closed) return;
    this.closed = true;
    const releases = this.owned.splice(0).reverse();
    for (const release of releases) {
      try {
        release();
      } catch (error) {
        console.error('[TerminalRegistry] Failed to release a cancelled terminal resource:', error);
      }
    }
  }
}

function measureAndFit(inst: TerminalInstance): void {
  // xterm.js exposes `_core._charSizeService` as an internal API for
  // re-measuring glyph widths — needed for Unicode 11+ alignment so
  // emoji output doesn't shear box-drawing borders (issue #602). It's
  // intentionally undocumented and not in the public types; the typed
  // alternative is to call `measure()` from a separately-vendored
  // service that re-exports the same hook.
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const charSizeService = (inst.term as any)['_core']?.['_charSizeService'];
  charSizeService?.measure();
  inst.fitAddon.fit();
}

export class TerminalRegistry {
  private instances = new Map<number, TerminalInstance>();
  private pending = new Map<number, PendingCreation>();
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
  // Set by `destroy()`. A module-level `listen()` that resolves after
  // teardown releases its handle instead of parking it in `unlistenFns`
  // for the rest of the process (issue #2016).
  private destroyed = false;

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
      // `destroy()` can land before this resolves (issue #2016). Parking
      // the handle in `unlistenFns` afterwards would leave a live
      // module-level listener nobody can ever reach again, so release it
      // here instead. The `destroyed` flag is the whole record — a
      // destroyed registry registers no further module-level listeners,
      // because its constructor has already run.
      if (this.destroyed) {
        unlisten();
        return;
      }
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
    const existing = this.instances.get(nodeId);
    if (existing) return existing;
    const inFlight = this.pending.get(nodeId);
    // Coalesce onto the creation in flight. `PendingCreation.promise` already
    // folds cancellation into its result, so this caller and the creator both
    // see `null` for a deleted node — handing a coalesced caller the raw
    // `doCreate` result would let it adopt an instance `dispose` already tore
    // down (issue #2016).
    if (inFlight) return inFlight.promise;
    // Stamp the per-node spawn-timing reference NOW (issue #602). This is
    // the moment the frontend first sees a fresh nodeId — the "frontend
    // realized it needs an xterm" anchor that the `xterm_mount` checkpoint
    // at `attachToDOM` will measure against. Doing it here (rather than
    // inside `doCreate`) means concurrent callers of `getOrCreate` for the
    // same nodeId all share the original stamp: `pending` already
    // deduplicates them, and only the first path reaches this line.
    this.nodeStartTimes.set(nodeId, performance.now());
    const request = new PendingCreation();
    // Map cancellation into the shared promise rather than at each call site,
    // so the creator and every coalesced caller resolve identically. A
    // deletion that lands after `doCreate` published the instance but before
    // this promise settles — a `notify()` subscriber deleting the node, for
    // one — must still yield `null` to all of them.
    request.promise = this.doCreate(nodeId, request).then((inst) =>
      request.cancelRequested ? null : inst,
    );
    this.pending.set(nodeId, request);
    try {
      return await request.promise;
    } finally {
      // Identity-checked: a later creation for the same node may already
      // own the slot (dispose releases the record, the next getOrCreate
      // installs a fresh one).
      if (this.pending.get(nodeId) === request) this.pending.delete(nodeId);
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
      // Bind the link-click guard to the element `open()` just created. It
      // lives on the persistent terminal element, so a later re-attach
      // re-parents it without re-binding.
      inst.links.attach(inst.term);
      // Capture Shift+Insert/browser paste before xterm converts it to PTY text.
      // This listener belongs to the persistent terminal element, not its pane.
      inst.term.element?.addEventListener('paste', (event) => {
        if (!inst.useNativeClipboardPaste) return;
        event.preventDefault();
        event.stopImmediatePropagation();
        this.pasteClipboard(nodeId).catch(console.error);
      }, true);
    } else {
      const termEl = inst.term.element;
      if (termEl && termEl.parentElement !== container) {
        container.appendChild(termEl);
      }
    }

    inst.attachedContainer = container;

    inst.resizeScheduler.attach(container);
    inst.resizeScheduler.fitNextFrame();

    requestAnimationFrame(() => {
      if (inst.attachedContainer !== container) return;
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

  async pasteClipboard(nodeId: number): Promise<void> {
    const inst = this.instances.get(nodeId);
    if (!inst) return;
    if (inst.useNativeClipboardPaste) {
      // Grok reads the Windows clipboard atomically. ConPTY text input loses
      // paste boundaries in its Win32 reader and can fragment multiline text.
      await api.writeToAgent(nodeId, '\x16');
      return;
    }
    // pbpaste bypasses WKWebView's clipboard-permission popup on macOS.
    const text = await api.readClipboard().catch(() => navigator.clipboard.readText());
    if (text) inst.term.paste(text);
  }

  detach(nodeId: number): void {
    const inst = this.instances.get(nodeId);
    if (!inst) return;

    inst.resizeScheduler.detach();
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
    // A deletion can land while `doCreate` is between awaits (issue
    // #2016). Mark the in-flight creation cancelled and release whatever
    // it already owns, so the lazy import / listener registrations that
    // settle afterwards cannot publish a terminal, keep a writer sink, a
    // font-size or theme entry, or a binary PTY Channel for a node that no
    // longer exists. The record itself is dropped by `getOrCreate` when
    // the creation settles, so this is not a deleted-id tombstone.
    const inFlight = this.pending.get(nodeId);
    if (inFlight) {
      inFlight.cancelRequested = true;
      inFlight.release();
    }
    // Drop the per-node spawn-timing stamp (issue #602). Without this
    // every spawn/delete cycle adds an entry to `nodeStartTimes` and
    // the map grows unbounded for long-running sessions that cycle
    // through many nodes. Cleared on the cancelled-creation path too —
    // that creation never reaches `dispose`'s instance branch.
    this.nodeStartTimes.delete(nodeId);
    const instance = this.instances.get(nodeId);
    if (instance) {
      instance.resizeScheduler.dispose();
      instance.links.dispose(); // allow-dispose — the only path that tears a terminal down is the deleted node's
      instance.unlisten();
      api.unsubscribeAgentOutput(nodeId).catch(() => {});
      terminalWebglPool.release(`agent:${nodeId}`);
      instance.term.dispose(); // allow-dispose — keyed by deleted-node IPC, the registry's only legit dispose path
      this.instances.delete(nodeId);
      this.writer.unregister(nodeId);
      this.fontSizeManager.unregister(nodeId);
      this.themeManager.unregister(nodeId);
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

  private async doCreate(nodeId: number, pending: PendingCreation): Promise<TerminalInstance | null> {
    // Deletion can land between any two awaits below, so the checkpoint
    // runs after each one: it releases what this creation already owns and
    // refuses to acquire anything further (issue #2016).
    const aborted = (): boolean => {
      if (!pending.cancelRequested) return false;
      pending.release();
      return true;
    };
    try {
      // Issue #1568 - lazy-load xterm + its addons + the unicode-width shim
      // behind a single dynamic import. Vite emits a separate chunk for each
      // of these modules, and they're only fetched on first terminal attach.
      // Without this, every byte of @xterm/xterm + 5 addons lands in the
      // desktop initial bundle even for users who never open a canvas.
      // `TerminalLinkRouter` rides here too: it is only reachable from
      // `doCreate`, and a static import of it costs the entry bundle enough
      // to break `check:bundle` (issue #2284 round 2).
      const [
        { Terminal },
        { FitAddon },
        { SerializeAddon },
        { SearchAddon },
        { WebLinksAddon },
        { loadUnicode11Widths },
        { TerminalLinkRouter },
      ] = await Promise.all([
        import('@xterm/xterm'),
        import('@xterm/addon-fit'),
        import('@xterm/addon-serialize'),
        import('@xterm/addon-search'),
        import('@xterm/addon-web-links'),
        import('./loadUnicode11Widths'),
        import('./TerminalLinkRouter'),
      ]);
      // Nothing has been acquired yet — bail before allocating an xterm
      // for a node that was deleted while the chunk was in flight.
      if (aborted()) return null;

      // Both link providers point at one router: it owns the click, so the
      // URL opens once no matter which provider xterm picks (or whether the
      // agent's TUI is also handling the press — see TerminalLinkRouter).
      const links = new TerminalLinkRouter();
      pending.own(() => links.dispose()); // allow-dispose — link router owned by this cancelled creation, not a live terminal

      const term = new Terminal({
        ...createTerminalOptions(),
        linkHandler: links.linkHandler,
      });
      // Owned immediately, before the first thing that can throw, so a
      // failure or a deletion anywhere in the rest of creation still tears
      // the xterm down (issue #2016). `release()` unwinds newest first, so
      // the terminal goes last — after the bookkeeping that points at it.
      pending.own(() => {
        term.dispose(); // allow-dispose — the node was deleted (or creation failed) before this terminal was ever published
      });

      const fitAddon = new FitAddon();
      term.loadAddon(fitAddon);
      const serializeAddon = new SerializeAddon();
      term.loadAddon(serializeAddon);
      const searchAddon = new SearchAddon();
      term.loadAddon(searchAddon);
      const webLinksAddon = new WebLinksAddon(links.openWebLink, links.webLinksOptions);
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
        links,
        unlisten: () => {},
        opened: false,
        resizeScheduler: new TerminalResizeScheduler(() => measureAndFit(instance)),
        attachedContainer: null,
        onFindRequest: null,
        useNativeClipboardPaste: false,
      };

      // The scheduler is owned as soon as the instance exists, so a deletion
      // that lands on the next line cannot leave it running (issue #2016).
      pending.own(() => instance.resizeScheduler.dispose()); // allow-dispose — resize scheduler owned by this cancelled creation, not a live terminal

      // `term.write`'s callback fires when xterm has parsed the payload, so
      // the writer can cap how much unparsed work xterm holds at once
      // (issue #2018). Both arguments are required here: the writer only
      // counts in-flight payloads for a sink declared completion-aware.
      //
      // The unwind is owned BEFORE the three registrations, so a throw in
      // any of them still drops the entries the earlier ones created
      // (issue #2016). All three `unregister` calls are no-ops for a key
      // that was never registered.
      pending.own(() => {
        this.writer.unregister(nodeId);
        this.fontSizeManager.unregister(nodeId);
        this.themeManager.unregister(nodeId);
      });
      this.writer.register(
        nodeId,
        (data, done) => term.write(data, done),
        { completionAware: true },
      );
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
      pending.own(() => unlisten());
      if (aborted()) return null;

      // Binary PTY path (issue #1385). Production bytes arrive here as a
      // `Uint8Array` with no Base64/JSON. The `agent-output` listener
      // above stays for test injection (`line`). Pre-subscribe bytes are
      // buffered on the backend OutputSink, not this JSON event.
      // `TerminalWriter` still rAF-batches the display write.
      const outputSubscribe = api.subscribeAgentOutput(nodeId, (bytes) => {
        this.writer.append(nodeId, bytes);
      }).catch(console.error);
      // The unsubscribe is chained AFTER the subscribe so a deletion that
      // lands mid-registration still drops the Channel the backend is in
      // the middle of creating (issue #2016) — unsubscribing first would
      // race the registration and leave the Channel alive. The Rust
      // `unsubscribe_agent_output` is idempotent, so the ordering is the
      // only thing that matters here.
      pending.own(() => {
        void outputSubscribe
          .then(() => api.unsubscribeAgentOutput(nodeId))
          .catch(() => {});
      });

      const unlistenSerialize = await listen<SerializeTerminalRequestPayload>('serialize-terminal-request', (event) => {
        if (event.payload.node_id === nodeId) {
          const snapshot = instance.serializeAddon.serialize({ scrollback: 200 });
          api.submitTerminalSnapshot(event.payload.request_id, snapshot).catch(console.error);
        }
      });
      pending.own(() => unlistenSerialize());
      if (aborted()) return null;

      term.onData((data) => {
        api.writeToAgent(nodeId, data).catch(console.error);
      });

      term.attachCustomKeyEventHandler((ev: KeyboardEvent) => {
        if (ev.type !== 'keydown') return true;

        const nativeShiftInsert = instance.useNativeClipboardPaste
          && ev.key === 'Insert' && ev.shiftKey && !ev.ctrlKey && !ev.altKey && !ev.metaKey;
        const action = nativeShiftInsert ? 'paste' : resolveKeyAction({
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
            this.pasteClipboard(nodeId).catch(console.error);
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

      // `finally`, not a plain sequence: a throw from one unlisten must not skip
      // the other, or `dispose(nodeId)` would leave a live listener behind.
      const combinedUnlisten: UnlistenFn = () => {
        try {
          unlisten();
        } finally {
          unlistenSerialize();
        }
      };
      instance.unlisten = combinedUnlisten;

      if (aborted()) return null;
      this.instances.set(nodeId, instance);
      // Ownership of everything acquired above moves to `instances`, which
      // `dispose(nodeId)` tears down. Closing the record here also means a
      // later `own()` would release rather than park — the creation is done.
      pending.commit();
      this.notify();
      return instance;
    } catch (e) {
      console.error(`[TerminalRegistry] Failed to create terminal for ${nodeId}`, e);
      // A failure after the terminal / writer / theme / listener
      // registrations leaves nothing behind: the record unwinds every
      // resource it owns (issue #2016). Before that, the catch cleared only
      // the spawn-timing stamp, so a failed create kept a half-built xterm,
      // a writer sink, font-size and theme entries and — once the binary
      // subscribe had started — a live PTY Channel.
      pending.release();
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
    // Set first: a module-level `listen()` that resolves after this point
    // releases its handle instead of appending to `unlistenFns`
    // (issue #2016).
    this.destroyed = true;
    // Creations still in flight keep no instance for `dispose` to find, so
    // they are cancelled and unwound through the same path first.
    for (const pending of [...this.pending.values()]) {
      pending.cancelRequested = true;
      pending.release();
    }
    for (const nodeId of [...this.instances.keys()]) {
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
