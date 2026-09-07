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

export interface BuildRunInstance {
  sessionId: number;
  mode: BuildRunMode;
  useWorktree: boolean;
  term: Terminal;
  fitAddon: FitAddon;
  /** Unlisten for the `build-run-output-{sessionId}` test-injection listener.
   *  Released by `disposeInstance`. Production bytes use the binary Channel. */
  outputUnlisten: UnlistenFn | null;
  /** Unlisten for the `build-run-exited-{sessionId}` listener. Released by
   *  `disposeInstance`. The listener closure checks the per-instance
   *  generation against the current session generation so stale exit events
   *  from previous PTY lifecycles are filtered out. */
  exitUnlisten: UnlistenFn | null;
  /** Generation token for this PTY lifecycle. Matches the module-level
   *  `sessionGenerations.get(sessionId)` at attach time. The exit listener
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
}

interface PendingCreation {
  promise: Promise<BuildRunInstance | null>;
  cancelRequested: boolean;
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
  /** Per-sessionId Promise chains that serialize `attach`. Each `attach`
   *  awaits the previous chain entry before doing its own work, so mode-
   *  conflict cleanup sees the previous attach's fully-settled state. */
  private sessionLocks = new Map<number, Promise<unknown>>();
  /** Explicit disposal removes the JS instance synchronously, but backend
   *  PTY/channel teardown is asynchronous. New attaches await this per-
   *  session chain before creating or spawning a replacement. */
  private sessionTeardowns = new Map<number, Promise<void>>();
  /** Per-sessionId generation counter. Incremented on each successful
   *  `doCreate`; exit listeners compare their captured value against the
   *  current value to drop stale events from previous PTY lifecycles. */
  private sessionGenerations = new Map<number, number>();
  // Issue #734: live-update the xterm.js palette on theme flips. Subscribes
  // to theme.ts's pub/sub at construction; walks `entries` on every flip;
  // releases its listener in destroy(). Separate from TerminalRegistry's
  // own ThemeManager because the two registries have independent
  // lifecycles (a build-run X-button close should NOT unregister an agent
  // terminal from the agent registry's theme map).
  private themeManager = new ThemeManager();

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
        return this.getOrCreate(sessionId, mode, useWorktree);
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
    // Serialize per sessionId so concurrent attaches (React 18 StrictMode
    // double-mount, fast Build→Run clicks) see each other's settled state
    // before doing mode-conflict cleanup.
    const previousLock = this.sessionLocks.get(sessionId) ?? Promise.resolve();
    let release!: () => void;
    const ourLock = new Promise<void>((resolve) => { release = resolve; });
    this.sessionLocks.set(sessionId, previousLock.then(() => ourLock));
    await previousLock;
    try {
      if (signal?.aborted) return null;
      await this.awaitSessionTeardown(sessionId);
      if (signal?.aborted) return null;
      // Mode-conflict cleanup: any sibling instance for this sessionId with
      // a different (mode, useWorktree) is disposed BEFORE we spawn a new
      // PTY. Serialization above guarantees the sibling is fully settled
      // (post-instances.set) before we run this scan.
      const targetKey = instanceKey(sessionId, mode, useWorktree);
      for (const [key, inst] of this.instances) {
        if (inst.sessionId === sessionId && key !== targetKey) {
          await this.disposeInstance(key);
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
          await this.disposeInstance(instanceKey(sessionId, mode, useWorktree));
        }
        return null;
      }
      return this.attachToDOM(inst, container);
    } finally {
      release();
      // Tidy up the lock chain if we're the last one queued. Keeps the
      // Map from growing for long-lived sessions that touch many nodes.
      if (this.sessionLocks.get(sessionId) === ourLock) {
        this.sessionLocks.delete(sessionId);
      }
    }
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
          // `dispose()` can run while build_run is resolving. The backend
          // only registers the PTY when build_run returns, so close it now
          // that the late resolution proves the process exists.
          await api.closeBuildRun(inst.sessionId).catch(() => {});
        } else {
          inst.ptyAlive = true;
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
    if (inst.disposed || this.instances.get(key) !== inst) return null;

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
        return pending.promise.then(() => undefined);
      }
    }
    return Promise.resolve();
  }

  private awaitSessionTeardown(sessionId: number): Promise<void> {
    return this.sessionTeardowns.get(sessionId) ?? Promise.resolve();
  }

  private queueSessionTeardown(sessionId: number, startTeardown: () => Promise<void>): Promise<void> {
    const previous = this.sessionTeardowns.get(sessionId);
    // Start the first teardown synchronously so the explicit-close path keeps
    // its immediate backend call. Later teardowns wait for the prior close and
    // channel unsubscribe to settle before they invoke their own IPC.
    const current = previous
      ? previous.catch(() => {}).then(startTeardown).catch(() => {})
      : startTeardown().catch(() => {});
    this.sessionTeardowns.set(sessionId, current);
    current.then(() => {
      if (this.sessionTeardowns.get(sessionId) === current) {
        this.sessionTeardowns.delete(sessionId);
      }
    }).catch(() => {});
    return current;
  }

  private disposeInstance(key: string): Promise<void> {
    const inst = this.instances.get(key);
    if (!inst) return Promise.resolve();

    inst.disposed = true;
    inst.attachedContainer = null;
    inst.resizeScheduler.dispose();
    terminalWebglPool.release(`buildRun:${key}`);
    if (inst.outputUnlisten) inst.outputUnlisten();
    if (inst.exitUnlisten) inst.exitUnlisten();
    inst.writer.unregister(inst.sessionId);
    // Issue #734: symmetric with doCreate's register — same composite key,
    // so a future flip doesn't push a stale palette into a dead xterm.
    this.themeManager.unregister(key);
    const ptyWasAlive = inst.ptyAlive;
    if (ptyWasAlive) {
      // Kill the Rust PTY. Only fire this if we believe one is alive —
      // otherwise a "double X click" or a stale close would be a no-op
      // on Rust, but skipping the round-trip is cheaper and clearer.
      inst.ptyAlive = false;
    }
    inst.term.dispose(); // allow-dispose — explicit X-button close; the React lifecycle calls `detach`, never this path
    this.instances.delete(key);

    return this.queueSessionTeardown(inst.sessionId, async () => {
      const teardownTasks: Promise<unknown>[] = [api.unsubscribeBuildRunOutput(inst.sessionId)];
      if (ptyWasAlive) teardownTasks.push(api.closeBuildRun(inst.sessionId));
      await Promise.all(teardownTasks.map(task => task.catch(() => {})));
    });
  }

  destroy(): void {
    for (const pending of this.pending.values()) {
      pending.cancelRequested = true;
    }
    for (const key of [...this.instances.keys()]) {
      this.disposeInstance(key);
    }
    // Keep the latest per-session generations as tombstones. A delayed exit
    // event from a disposed PTY must not match a newly reopened PTY that
    // happens to reuse the same session id.
    this.sessionLocks.clear();
    // Issue #734: release the theme-listener so a destroyed build-run
    // registry doesn't keep firing flips into a now-empty entry map.
    this.themeManager.destroy();
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
      const generation = (this.sessionGenerations.get(sessionId) ?? 0) + 1;
      this.sessionGenerations.set(sessionId, generation);

      let inst: BuildRunInstance;
      inst = {
        sessionId,
        mode,
        useWorktree,
        term,
        fitAddon,
        outputUnlisten: null,
        exitUnlisten: null,
        generation,
        writer,
        opened: false,
        attachedContainer: null,
        resizeScheduler: new TerminalResizeScheduler(() => measureAndFit(inst)),
        ptyAlive: false,
        disposed: false,
      };

      // Issue #734: register with ThemeManager so a later theme flip
      // pushes the matching xterm palette into term.options.theme. Keyed
      // by the same composite instance-key the registry uses for its
      // `instances` Map (sessionId + mode + useWorktree), so the
      // unregister path is symmetric.
      this.themeManager.register(instanceKey(sessionId, mode, useWorktree), term);

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

      api.subscribeBuildRunOutput(sessionId, (bytes) => {
        writer.append(sessionId, bytes);
      }).catch(console.error);

      // Per-instance exit listener (NOT per-sessionId module-level). Each
      // instance owns its own subscription and unlistens in disposeInstance.
      // The closure checks `currentGeneration === inst.generation` to drop
      // stale events from a previous PTY lifecycle: a dispose+reopen within
      // the listen-resolution window would otherwise mis-fire the previous
      // reader thread's exit event onto the new instance.
      const exitEventName = `build-run-exited-${sessionId}`;
      const exitUnlisten = await listen<BuildRunExitedPayload>(exitEventName, () => {
        if (this.instances.get(instanceKey(sessionId, mode, useWorktree)) !== inst) return;
        if ((this.sessionGenerations.get(sessionId) ?? -1) !== generation) return;
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
        await this.disposeInstance(key);
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
