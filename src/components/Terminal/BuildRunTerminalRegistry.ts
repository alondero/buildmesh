import type { Terminal } from '@xterm/xterm';
import type { FitAddon } from '@xterm/addon-fit';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import * as api from '../../lib/tauri';
import { createTerminalOptions } from './terminalConfig';
import { terminalWebglPool } from './WebglRendererPool';
import { TerminalWriter } from './TerminalWriter';
import { TerminalResizeScheduler } from './TerminalResizeScheduler';
import { decodeBase64Bytes } from '../../lib/base64';
import type { BuildRunOutputPayload } from '../../types/generated/BuildRunOutputPayload';
import type { BuildRunExitedPayload } from '../../types/generated/BuildRunExitedPayload';

export type { BuildRunOutputPayload };
import { FontSizeManager } from './FontSizeManager';
import { ThemeManager } from './ThemeManager';
import { setTheme, type ThemeName } from '../../lib/theme';

/**
 * Sibling singleton for build/run terminal panes — mirrors `TerminalRegistry`
 * (used by the agent terminal) but is scoped to the BuildRun feature because:
 *
 * - Key namespace collision: `TerminalRegistry`'s shared writer is keyed by
 *   `nodeId` (agent session id). BuildRun uses `sessionId` which is the same
 *   numeric space — sharing would route agent output to the build-run xterm.
 * - Different input wiring: only Terminal mode is bidirectional; Build/Run are
 *   one-shot output streams.
 * - Different dedup key: `(sessionId, mode, useWorktree)` so a mode change
 *   forces a fresh PTY.
 *
 * Lifecycle contract (matches TerminalRegistry):
 * - `attach` — lazily creates the Terminal + spawns the PTY on first attach,
 *   moves the existing `.xterm` element into the new container on re-attach.
 * - `detach` — DOM-only, no terminal disposal, no PTY kill. Called from the
 *   React effect cleanup when a NodeCard unmounts (e.g. mesh switch).
 * - `dispose` — full teardown including PTY kill. Called from the X-button
 *   close handler ONLY.
 *
 * Concurrency invariants:
 * - `attach` is serialized per `sessionId` via a Promise chain. Two
 *   concurrent attaches on the same node (e.g. user clicks Build then Run
 *   within the listen-resolution window, or React 18 StrictMode double-mount)
 *   see each other's state and clean up mode-conflicting siblings before
 *   spawning a new PTY. Without this, both would race past
 *   `findKeysBySessionId` before either reached `instances.set`, and the
 *   Rust `HashMap::insert` would orphan the first PTY.
 * - The `build-run-exited-{sessionId}` sentinel is paired with a generation
 *   counter per sessionId so a late exit event from a previous PTY lifecycle
 *   cannot misfire on a freshly-reopened instance.
 *
 * Scope note: this registry is JS-process-scoped. It assumes parity with the
 * Rust `BUILD_RUN_REGISTRY` — a Tauri runtime restart or app cold start
 * wipes JS state and the next attach will spawn a fresh PTY. The Rust side's
 * `HashMap::insert` would orphan any stale entry in that case; out of scope
 * for this fix.
 */
export type BuildRunMode = 'build' | 'run' | 'terminal';
export type BuildRunLifecycle = 'idle' | 'spawning' | 'ready' | 'terminating' | 'terminated';

export interface BuildRunInstance {
  sessionId: number;
  mode: BuildRunMode;
  useWorktree: boolean;
  term: Terminal;
  fitAddon: FitAddon;
  /** Unlisten for the `build-run-output-{sessionId}` test-injection listener.
   *  Released by `disposeInstance`. Production bytes use the binary Channel. */
  outputUnlisten: UnlistenFn | null;
  /** Resolves once the backend output channel registration has completed. */
  outputSubscribe: Promise<void> | null;
  /** Unlisten for the `build-run-exited-{sessionId}` listener. Released by
   *  `disposeInstance`. The listener closure checks the per-instance
   *  generation against the current session generation so stale exit events
   *  from previous PTY lifecycles are filtered out. */
  exitUnlisten: UnlistenFn | null;
  /** Generation token for this PTY lifecycle. Matches the module-level
   *  session state at attach time. The exit listener
   *  no-ops if the generation has been superseded by a subsequent spawn. */
  generation: number;
  /** Per-instance output writer. Same shape as TerminalRegistry's per-instance
   *  writer — we own one per build-run instance because the shared registry
   *  writer is keyed by nodeId and the namespaces collide. */
  writer: TerminalWriter;
  opened: boolean;
  attachedContainer: HTMLElement | null;
  resizeScheduler: TerminalResizeScheduler;
  /** True when the Rust PTY is alive for this session. Set after `api.buildRun`
   *  resolves (NOT before — see attachToDOM), cleared by `dispose` or by the
   *  `build-run-exited-{sessionId}` sentinel when the shell exits naturally. */
  ptyAlive: boolean;
  /** Set before the instance is removed so an in-flight `buildRun` resolution
   *  can close the PTY it just created instead of reviving an unreachable
   *  registry entry. */
  disposed: boolean;
  /** Explicit lifecycle state. Async PTY completion may only advance this
   * state; disposal moves it to terminating before the instance leaves the
   * registry. */
  lifecycle: BuildRunLifecycle;
}

interface PendingCreation {
  promise: Promise<BuildRunInstance | null>;
  cancelRequested: boolean;
}

interface SessionState {
  /** One queue owns attach, spawn completion, unsubscribe, and PTY close. */
  queue: Promise<void>;
  busy: boolean;
  /** Monotonic tombstone used by exit listeners across reopen cycles. */
  generation: number;
}

function instanceKey(sessionId: number, mode: BuildRunMode, useWorktree: boolean): string {
  return `${sessionId}|${mode}|${useWorktree}`;
}

function modeBanner(mode: BuildRunMode, useWorktree: boolean): string {
  const prefix =
    mode === 'terminal' ? 'Opening terminal' : mode === 'build' ? 'Building' : 'Running';
  return `${prefix}${useWorktree ? ' in worktree' : ''}...\r\n`;
}

function payloadToBytes(payload: string | BuildRunOutputPayload): string | Uint8Array {
  if (typeof payload === 'string') return payload;
  // `!= null` catches both Rust's `None` (serialised as `null`) AND a
  // test-constructed literal that omits the field entirely (TypeScript
  // widens the missing key to `undefined` at runtime).
  if (payload.data != null) return decodeBase64Bytes(payload.data);
  return '';
}

/** Centralized fit() that ALSO re-measures char widths — needed for
 *  Unicode 11+ glyph alignment so emoji output doesn't shear box-drawing
 *  borders. Mirrors TerminalRegistry.ts's `measureAndFit`. */
function measureAndFit(inst: BuildRunInstance): void {
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  const charSizeService = (inst.term as any)['_core']?.['_charSizeService'];
  charSizeService?.measure();
  inst.fitAddon.fit();
}

export class BuildRunTerminalRegistry {
  private instances = new Map<string, BuildRunInstance>();
  private pending = new Map<string, PendingCreation>();
  /** Per-session lifecycle queue and generation tombstone. Keeping both in
   *  one record prevents spawn completion from escaping the teardown chain. */
  private sessions = new Map<number, SessionState>();
  // Issue #734: live-update the xterm.js palette on theme flips. Subscribes
  // to theme.ts's pub/sub at construction; walks `entries` on every flip;
  // releases its listener in destroy(). Separate from TerminalRegistry's
  // own ThemeManager because the two registries have independent
  // lifecycles (a build-run X-button close should NOT unregister an agent
  // terminal from the agent registry's theme map).
  private themeManager = new ThemeManager();
  // Live font-size fan-out, mirroring TerminalRegistry. Without this the
  // build/run panes only read the size at creation, so Ctrl/Cmd+/- and the
  // title-bar zoom slider left them at their old text size. Keyed by the
  // composite instance key (strings, unlike the agent registry's node ids).
  private fontSizeManager = new FontSizeManager<string>();

  getInstance(sessionId: number, mode: BuildRunMode, useWorktree: boolean): BuildRunInstance | undefined {
    return this.instances.get(instanceKey(sessionId, mode, useWorktree));
  }

  async getOrCreate(
    sessionId: number,
    mode: BuildRunMode,
    useWorktree: boolean,
  ): Promise<BuildRunInstance | null> {
    const key = instanceKey(sessionId, mode, useWorktree);
    const existing = this.instances.get(key);
    if (existing) return existing;
    const pending = this.pending.get(key);
    if (pending) {
      // An explicit close may race the lazy xterm/listener setup. Let that
      // creation finish its own cleanup before a new attach starts; otherwise
      // the new request could coalesce with a creation that is already slated
      // for disposal.
      if (pending.cancelRequested) {
        await pending.promise;
        return null;
      }
      return pending.promise;
    }
    const request = {
      promise: undefined as unknown as Promise<BuildRunInstance | null>,
      cancelRequested: false,
    } satisfies PendingCreation;
    request.promise = this.doCreate(sessionId, mode, useWorktree, request);
    this.pending.set(key, request);
    try {
      return await request.promise;
    } finally {
      if (this.pending.get(key) === request) this.pending.delete(key);
    }
  }

  /**
   * Attach the singleton xterm + PTY for this `(sessionId, mode, useWorktree)`
   * into `container`. Reuses an existing instance on re-attach (mesh switch,
   * pane reorder) so scrollback and the live PTY survive.
   *
   * `signal` is optional; if provided and already aborted, attach returns
   * without spawning a PTY (avoids the React-effect-unmount race where the
   * component has already been told to clean up but the in-flight listen()
   * hasn't resolved yet).
   */
  async attach(
    sessionId: number,
    mode: BuildRunMode,
    useWorktree: boolean,
    container: HTMLElement,
    signal?: AbortSignal,
  ): Promise<BuildRunInstance | null> {
    // One queue owns the complete session lifecycle. In particular, a
    // disposal requested while buildRun is pending is ordered after that
    // spawn's resolution and before the next attach can create a replacement.
    return this.enqueueSession(sessionId, async () => {
      if (signal?.aborted) return null;
      // Mode-conflict cleanup: any sibling instance for this sessionId with
      // a different (mode, useWorktree) is disposed BEFORE we spawn a new
      // PTY. The inline path is safe because this callback already owns the
      // session queue.
      const targetKey = instanceKey(sessionId, mode, useWorktree);
      for (const [key, inst] of this.instances) {
        if (inst.sessionId === sessionId && key !== targetKey) {
          await this.disposeInstanceInline(key);
        }
      }

      const inst = await this.getOrCreate(sessionId, mode, useWorktree);
      if (!inst) return null;
      // If the React effect already aborted during our await, bail out
      // before opening the xterm into a deleted container or spawning a PTY
      // that no one will display. The instance must be disposed here because
      // `dispose()` may have run before lazy creation put it in the map.
      if (signal?.aborted) {
        if (this.instances.get(instanceKey(sessionId, mode, useWorktree)) === inst) {
          await this.disposeInstanceInline(instanceKey(sessionId, mode, useWorktree));
        }
        return null;
      }
      return this.attachToDOM(inst, container);
    });
  }

  private async attachToDOM(inst: BuildRunInstance, container: HTMLElement): Promise<BuildRunInstance | null> {
    const key = instanceKey(inst.sessionId, inst.mode, inst.useWorktree);
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

    inst.resizeScheduler.attach(container);
    inst.resizeScheduler.fitNextFrame();

    if (wasFreshOpen) {
      // First open only — write the banner and spawn the PTY. On re-attach
      // (user navigated back) we skip this so the scrollback isn't cluttered
      // with a second banner, and so the existing PTY isn't overwritten.
      inst.term.write(modeBanner(inst.mode, inst.useWorktree));
      inst.lifecycle = 'spawning';
      // Set ptyAlive=true ONLY after api.buildRun resolves. Setting it
      // synchronously here races with a quick X-click: the user could
      // click X between this line and the IPC returning, and dispose
      // would see ptyAlive=true and fire closeBuildRun against a
      // buildRun that's still in flight on Rust. Moving the flag flip
      // into the success path makes the JS-side flag match the Rust-side
      // reality.
      // Keep the per-session attach lock held until the spawn settles. A
      // mode switch must not start a second PTY while this one is still in
      // flight: `close_build_run` is keyed only by session id and could close
      // the replacement process or miss the late-created original.
      try {
        await api.buildRun(inst.sessionId, inst.mode);
        if (inst.disposed || this.instances.get(key) !== inst) {
          // The session queue owns the subsequent teardown. Mark the PTY
          // alive here so the queued disposer closes the process after this
          // spawn resolves; a direct close would race a replacement attach.
          inst.ptyAlive = true;
          inst.lifecycle = 'terminating';
          await this.finishDisposal(inst);
        } else {
          inst.ptyAlive = true;
          inst.lifecycle = 'ready';
        }
      } catch (err) {
        if (!inst.disposed && this.instances.get(key) === inst) {
          inst.term.write(`\r\nError: ${String(err)}\r\n`);
        }
      }
    }

    // Explicit close may have happened while the first spawn was pending.
    // Do not schedule repaint/renderer work against the disposed xterm after
    // the late-resolution cleanup above has finished.
    if (inst.disposed || this.instances.get(key) !== inst || inst.lifecycle === 'terminating') return null;

    requestAnimationFrame(() => {
      if (inst.attachedContainer !== container) return;
      // Only auto-scroll-to-tail on the first open. On re-attach the user
      // may have scrolled back to read history; forcing the tail here would
      // silently destroy that position (and flash the jump-to-latest pill).
      if (wasFreshOpen) inst.term.scrollToBottom();
      // refresh() repaints the accumulated scrollback on re-attach so the
      // user sees everything that streamed in while the xterm was detached.
      inst.term.refresh(0, inst.term.rows - 1);
    });

    // GPU context budget: this pane just became visible, so it earns a
    // WebGL renderer (LRU-capped pool; hidden panes fall back to DOM).
    terminalWebglPool.activate(
      `buildRun:${instanceKey(inst.sessionId, inst.mode, inst.useWorktree)}`,
      inst.term,
    );

    return inst.disposed || this.instances.get(key) !== inst ? null : inst;
  }

  detach(sessionId: number, mode: BuildRunMode, useWorktree: boolean): void {
    const key = instanceKey(sessionId, mode, useWorktree);
    const inst = this.instances.get(key);
    if (!inst) return;

    inst.resizeScheduler.detach();
    inst.term.element?.remove();
    inst.attachedContainer = null;
    terminalWebglPool.release(`buildRun:${key}`);
  }

  /** Full teardown — called from the X button. Kills the Rust PTY,
   *  disposes the xterm, removes from the instances map. */
  dispose(sessionId: number, mode: BuildRunMode, useWorktree: boolean): Promise<void> {
    const key = instanceKey(sessionId, mode, useWorktree);
    if (this.instances.has(key)) {
      return this.disposeInstance(key);
    } else {
      const pending = this.pending.get(key);
      if (pending) {
        pending.cancelRequested = true;
        return this.awaitSessionQueue(sessionId);
      }
    }
    return Promise.resolve();
  }

  private awaitSessionQueue(sessionId: number): Promise<void> {
    return this.sessions.get(sessionId)?.queue ?? Promise.resolve();
  }

  private enqueueSession<T>(sessionId: number, operation: () => Promise<T>): Promise<T> {
    const state = this.sessions.get(sessionId) ?? { queue: Promise.resolve(), busy: false, generation: 0 };
    const run = () => {
      try { return operation(); }
      catch (error) { return Promise.reject(error); }
    };
    const result = state.busy ? state.queue.catch(() => {}).then(run) : run();
    state.busy = true;
    state.queue = result.then(() => { state.busy = false; }, () => { state.busy = false; });
    this.sessions.set(sessionId, state);
    return result;
  }

  /** Remove the instance synchronously and return it for queued teardown. */
  private beginDisposal(key: string): BuildRunInstance | null {
    const inst = this.instances.get(key);
    if (!inst || inst.lifecycle === 'terminating' || inst.lifecycle === 'terminated') return null;

    inst.disposed = true;
    inst.lifecycle = 'terminating';
    inst.attachedContainer = null;
    inst.resizeScheduler.dispose();
    terminalWebglPool.release(`buildRun:${key}`);
    if (inst.outputUnlisten) inst.outputUnlisten();
    if (inst.exitUnlisten) inst.exitUnlisten();
    inst.writer.unregister(inst.sessionId);
    // Issue #734: symmetric with doCreate's register — same composite key,
    // so a future flip doesn't push a stale palette into a dead xterm.
    this.themeManager.unregister(key);
    this.fontSizeManager.unregister(key);
    inst.term.dispose(); // allow-dispose — explicit X-button close; the React lifecycle calls `detach`, never this path
    this.instances.delete(key);
    return inst;
  }

  private async finishDisposal(inst: BuildRunInstance): Promise<void> {
    if (inst.lifecycle === 'terminated') return;
    // `attachToDOM` is part of the same session queue, so by the time this
    // runs a late buildRun completion has already marked ptyAlive. This is the
    // single place that closes the PTY and drops its output channel.
    const teardownTasks: Promise<unknown>[] = [api.unsubscribeBuildRunOutput(inst.sessionId)];
    if (inst.ptyAlive) {
      inst.ptyAlive = false;
      teardownTasks.push(api.closeBuildRun(inst.sessionId));
    }
    await Promise.all(teardownTasks.map(task => task.catch(() => {})));
    inst.lifecycle = 'terminated';
  }

  private disposeInstanceInline(key: string): Promise<void> {
    const inst = this.beginDisposal(key);
    return inst ? this.finishDisposal(inst) : Promise.resolve();
  }

  private disposeInstance(key: string): Promise<void> {
    const inst = this.beginDisposal(key);
    if (!inst) return Promise.resolve();

    return this.enqueueSession(inst.sessionId, () => this.finishDisposal(inst));
  }

  destroy(): void {
    for (const pending of this.pending.values()) {
      pending.cancelRequested = true;
    }
    for (const key of [...this.instances.keys()]) {
      this.disposeInstance(key);
    }
    // Keep each session's generation tombstone. A delayed exit event from a
    // disposed PTY must not match a newly reopened PTY that reuses its id.
    // Issue #734: release the theme-listener so a destroyed build-run
    // registry doesn't keep firing flips into a now-empty entry map.
    this.themeManager.destroy();
    this.fontSizeManager.destroy();
  }

  /**
   * Push the named theme to every live build-run terminal AND the
   * <html data-theme> attribute. Mirrors TerminalRegistry.applyTheme()
   * so both registries present the same single entry point for code
   * (and tests) that wants to flip the theme without going through
   * `setTheme` directly. The actual palette push is delivered by this
   * registry's own ThemeManager, which subscribes to theme.ts's
   * pub/sub at construction and walks its entry map on every
   * `changed` event — so the live xterm updates happen even when a
   * caller flips the theme via `setTheme(...)` without going through
   * this method. Issue #734.
   */
  applyTheme(theme: ThemeName): void {
    setTheme(theme);
  }

  private async doCreate(
    sessionId: number,
    mode: BuildRunMode,
    useWorktree: boolean,
    pending: PendingCreation,
  ): Promise<BuildRunInstance | null> {
    try {
      // Issue #1568 - lazy-load xterm + FitAddon + the unicode-width shim.
      // Mirrors TerminalRegistry.doCreate; keep these two sites consistent so
      // the same chunk is shared on the wire.
      const [
        { Terminal },
        { FitAddon },
        { loadUnicode11Widths },
      ] = await Promise.all([
        import('@xterm/xterm'),
        import('@xterm/addon-fit'),
        import('./loadUnicode11Widths'),
      ]);

      // `createTerminalOptions` (not the static `TERMINAL_OPTIONS`) so the
      // build-run terminal respects the user's font-size preference set
      // via Ctrl+/- in the agent terminal (issue: was always rendered at
      // xterm's internal default — see Terminal.tsx / TerminalRegistry.ts).
      const term = new Terminal(createTerminalOptions());
      const fitAddon = new FitAddon();
      term.loadAddon(fitAddon);
      // Match modern CLIs' Unicode 11+ glyph widths so emoji output doesn't
      // shear box-drawing borders (xterm defaults to Unicode 6 widths).
      // createTerminalOptions sets allowProposedApi, which this addon requires.
      loadUnicode11Widths(term);
      // Issue #1122: the WebGL renderer is attached LAZILY by
      // `WebglRendererPool.activate` on DOM attach (same GPU context budget
      // as the agent terminal — see TerminalRegistry.ts / WebglRendererPool.ts).

      // Per-instance writer (NOT the shared registry writer — see class
      // header comment about key namespace collision).
      const writer = new TerminalWriter();
      writer.register(sessionId, (data) => term.write(data));

      // Bump the per-sessionId generation. Each doCreate for the same
      // sessionId increments — the exit listener installed below captures
      // this value and ignores events whose payload matches an OLDER
      // generation (which would mean the PTY that died is not the one
      // we're currently showing).
      const session = this.sessions.get(sessionId) ?? { queue: Promise.resolve(), busy: false, generation: 0 };
      const generation = session.generation + 1;
      session.generation = generation;
      this.sessions.set(sessionId, session);

      let inst: BuildRunInstance;
      inst = {
        sessionId,
        mode,
        useWorktree,
        term,
        fitAddon,
        outputUnlisten: null,
        outputSubscribe: null,
        exitUnlisten: null,
        generation,
        writer,
        opened: false,
        attachedContainer: null,
        resizeScheduler: new TerminalResizeScheduler(() => measureAndFit(inst)),
        ptyAlive: false,
        disposed: false,
        lifecycle: 'idle',
      };

      // Issue #734: register with ThemeManager so a later theme flip
      // pushes the matching xterm palette into term.options.theme. Keyed
      // by the same composite instance-key the registry uses for its
      // `instances` Map (sessionId + mode + useWorktree), so the
      // unregister path is symmetric.
      this.themeManager.register(instanceKey(sessionId, mode, useWorktree), term);
      // Same composite key as ThemeManager so the unregister path is
      // symmetric. `measureAndFit(inst)` re-measures glyph widths and refits
      // the pane after a zoom change.
      this.fontSizeManager.register(instanceKey(sessionId, mode, useWorktree), term, () => measureAndFit(inst));

      // Wire keystroke + resize handlers for interactive Terminal mode only.
      // Build/Run is one-way output — user input is ignored. Mirrors the
      // pattern in TerminalRegistry.ts for the agent terminal.
      if (mode === 'terminal') {
        term.onData((data) => {
          api.writeToBuildRun(sessionId, data).catch((err) => {
            // The PTY may have exited (e.g. user typed `exit`) — swallow
            // the "not running" error since it's expected. Without the
            // Rust-side `registry.remove(&node_id)` on EOF (see
            // build_run.rs reader thread), this catch is the only thing
            // protecting users from silently vanished keystrokes — make
            // sure the Rust fix stays in place.
            if (err !== 'Build run not running') {
              console.error('[BuildRunTerminalRegistry] write_to_build_run failed:', err);
            }
          });
        });
        term.onResize(({ cols, rows }) => {
          api.resizeBuildRun(sessionId, rows, cols).catch(() => {});
        });
      }

      // JSON event fallback (issue #1393). Production bytes arrive on the
      // binary Channel below; this listener stays for test injection
      // (`build-run-output-{sessionId}` string or `{ data }` payloads).
      const outputEventName = `build-run-output-${sessionId}`;
      const outputUnlisten = await listen<string | BuildRunOutputPayload>(outputEventName, (event) => {
        const data = payloadToBytes(event.payload);
        if (data !== '') writer.append(sessionId, data);
      });
      inst.outputUnlisten = outputUnlisten;

      inst.outputSubscribe = api.subscribeBuildRunOutput(sessionId, (bytes) => {
        writer.append(sessionId, bytes);
      }).catch((error) => { console.error(error); });
      // Do not publish an instance until the backend channel registration has
      // settled. This lets disposal call unsubscribe in deterministic order
      // without a second free-floating promise.
      await inst.outputSubscribe;

      // Per-instance exit listener (NOT per-sessionId module-level). Each
      // instance owns its own subscription and unlistens in disposeInstance.
      // The closure checks `currentGeneration === inst.generation` to drop
      // stale events from a previous PTY lifecycle: a dispose+reopen within
      // the listen-resolution window would otherwise mis-fire the previous
      // reader thread's exit event onto the new instance.
      const exitEventName = `build-run-exited-${sessionId}`;
      const exitUnlisten = await listen<BuildRunExitedPayload>(exitEventName, (event) => {
        // Primary contract: the payload's `generation` field identifies
        // which incarnation's reader saw EOF (round-5 review finding
        // #3 — added the field to BuildRunExitedPayload). Without
        // this, the listener can only know whether it IS the current
        // instance — it can't tell whether the exit event is from
        // its own incarnation or a previous one's late EOF that
        // crossed paths with a replacement.
        if (event.payload.generation !== generation) return;
        // Secondary sanity checks (defense in depth):
        if (this.instances.get(instanceKey(sessionId, mode, useWorktree)) !== inst) return;
        if ((this.sessions.get(sessionId)?.generation ?? -1) !== generation) return;
        inst.ptyAlive = false;
        if (inst.attachedContainer) {
          inst.term.write('\r\n[process exited]\r\n');
        }
      });
      inst.exitUnlisten = exitUnlisten;

      const key = instanceKey(sessionId, mode, useWorktree);
      this.instances.set(key, inst);
      if (pending.cancelRequested) {
        // Put it through the same cleanup path as an already-mounted
        // instance. No PTY has been spawned yet, so this only releases the
        // terminal/listeners and prevents an unreachable registry entry.
        const cancelled = this.beginDisposal(key);
        if (cancelled) void this.enqueueSession(sessionId, () => this.finishDisposal(cancelled));
        return null;
      }

      return inst;
    } catch (e) {
      console.error(`[BuildRunTerminalRegistry] Failed to create terminal for ${sessionId}`, e);
      return null;
    }
  }
}

export const buildRunTerminalManager = new BuildRunTerminalRegistry();

// Expose globally for E2E tests, mirroring Terminal.tsx:27.
declare global {
  interface Window {
    __buildRunTerminalManager?: typeof buildRunTerminalManager;
  }
}
if (typeof window !== 'undefined') {
  window.__buildRunTerminalManager = buildRunTerminalManager;
}
