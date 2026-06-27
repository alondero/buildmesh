import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import * as api from '../../lib/tauri';
import { TERMINAL_OPTIONS } from './terminalConfig';
import { loadUnicode11Widths } from './loadUnicode11Widths';
import { TerminalWriter } from './TerminalWriter';
import { decodeBase64Bytes } from '../../lib/base64';

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
 * Scope note: this registry is JS-process-scoped. It assumes parity with the
 * Rust `BUILD_RUN_REGISTRY` — a Tauri runtime restart or app cold start
 * wipes JS state and the next attach will spawn a fresh PTY. The Rust side's
 * `HashMap::insert` would orphan any stale entry in that case; out of scope
 * for this fix.
 */
export type BuildRunMode = 'build' | 'run' | 'terminal';

interface BuildRunOutputPayload {
  data?: string;
}

export interface BuildRunInstance {
  sessionId: number;
  mode: BuildRunMode;
  useWorktree: boolean;
  term: Terminal;
  fitAddon: FitAddon;
  unlisten: UnlistenFn | null;
  /** Per-instance output writer. Same shape as TerminalRegistry's per-instance
   *  writer — we own one per build-run instance because the shared registry
   *  writer is keyed by nodeId and the namespaces collide. */
  writer: TerminalWriter;
  opened: boolean;
  attachedContainer: HTMLElement | null;
  resizeObserver: ResizeObserver | null;
  /** True when the Rust PTY is alive for this session. Set by first attach
   *  (when `api.buildRun` is called), cleared by `dispose` or by the
   *  `build-run-exited-{sessionId}` sentinel when the shell exits naturally. */
  ptyAlive: boolean;
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
  if (payload.data !== undefined) return decodeBase64Bytes(payload.data);
  return '';
}

export class BuildRunTerminalRegistry {
  private instances = new Map<string, BuildRunInstance>();
  private pending = new Map<string, Promise<BuildRunInstance | null>>();
  /** Per-sessionId module-level unlistens for `build-run-exited-{sessionId}`.
   *  Mirrors TerminalRegistry's module-level `agent-spawned` listener pattern
   *  (TerminalRegistry.ts:60-76). Installed on first `doCreate` for a given
   *  sessionId, released in `destroy()`. */
  private moduleUnlistens = new Map<number, UnlistenFn>();

  /** Find keys for any instance with this sessionId regardless of
   *  mode/useWorktree. Used by `attach` to clean up mode-conflicting
   *  siblings before creating a fresh one. */
  private findKeysBySessionId(sessionId: number): string[] {
    const keys: string[] = [];
    for (const [key, inst] of this.instances) {
      if (inst.sessionId === sessionId) keys.push(key);
    }
    return keys;
  }

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
    if (this.pending.has(key)) return this.pending.get(key)!;
    const promise = this.doCreate(sessionId, mode, useWorktree);
    this.pending.set(key, promise);
    try {
      return await promise;
    } finally {
      this.pending.delete(key);
    }
  }

  async attach(
    sessionId: number,
    mode: BuildRunMode,
    useWorktree: boolean,
    container: HTMLElement,
  ): Promise<BuildRunInstance | null> {
    // Mode-conflict cleanup: if the same sessionId has an instance under a
    // different (mode, useWorktree), dispose it so we don't leak a Rust PTY
    // for a stale command. (Only one build-run PTY per node_id can exist in
    // BUILD_RUN_REGISTRY at a time — a second `api.build_run` call would
    // overwrite it, leaking the previous master under an unreachable Arc.)
    for (const key of this.findKeysBySessionId(sessionId)) {
      if (key !== instanceKey(sessionId, mode, useWorktree)) {
        this.disposeInstance(key);
      }
    }

    const inst = await this.getOrCreate(sessionId, mode, useWorktree);
    if (!inst) return null;
    return this.attachToDOM(inst, container);
  }

  private attachToDOM(inst: BuildRunInstance, container: HTMLElement): BuildRunInstance {
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
        inst.fitAddon.fit();
      });
    });
    inst.resizeObserver.observe(container);

    if (wasFreshOpen) {
      // First open only — write the banner and spawn the PTY. On re-attach
      // (user navigated back) we skip this so the scrollback isn't cluttered
      // with a second banner, and so the existing PTY isn't overwritten.
      inst.term.write(modeBanner(inst.mode, inst.useWorktree));
      inst.ptyAlive = true;
      api.buildRun(inst.sessionId, inst.mode).catch((err) => {
        inst.term.write(`\r\nError: ${String(err)}\r\n`);
        inst.ptyAlive = false;
      });
    }

    requestAnimationFrame(() => {
      inst.fitAddon.fit();
      // Only auto-scroll-to-tail on the first open. On re-attach the user
      // may have scrolled back to read history; forcing the tail here would
      // silently destroy that position (and flash the jump-to-latest pill).
      if (wasFreshOpen) inst.term.scrollToBottom();
      // refresh() repaints the accumulated scrollback on re-attach so the
      // user sees everything that streamed in while the xterm was detached.
      inst.term.refresh(0, inst.term.rows - 1);
    });

    return inst;
  }

  detach(sessionId: number, mode: BuildRunMode, useWorktree: boolean): void {
    const inst = this.instances.get(instanceKey(sessionId, mode, useWorktree));
    if (!inst) return;

    if (inst.resizeObserver) {
      inst.resizeObserver.disconnect();
      inst.resizeObserver = null;
    }
    inst.term.element?.remove();
    inst.attachedContainer = null;
  }

  /** Full teardown — called from the X button. Kills the Rust PTY,
   *  disposes the xterm, removes from the instances map. */
  dispose(sessionId: number, mode: BuildRunMode, useWorktree: boolean): void {
    this.disposeInstance(instanceKey(sessionId, mode, useWorktree));
  }

  private disposeInstance(key: string): void {
    const inst = this.instances.get(key);
    if (!inst) return;

    if (inst.resizeObserver) {
      inst.resizeObserver.disconnect();
      inst.resizeObserver = null;
    }
    if (inst.unlisten) inst.unlisten();
    inst.writer.unregister(inst.sessionId);
    if (inst.ptyAlive) {
      // Kill the Rust PTY. Only fire this if we believe one is alive —
      // otherwise a "double X click" or a stale close would be a no-op
      // on Rust, but skipping the round-trip is cheaper and clearer.
      api.closeBuildRun(inst.sessionId).catch(() => {});
      inst.ptyAlive = false;
    }
    inst.term.dispose(); // allow-dispose — explicit X-button close; the React lifecycle calls `detach`, never this path
    this.instances.delete(key);
  }

  destroy(): void {
    for (const key of [...this.instances.keys()]) {
      this.disposeInstance(key);
    }
    for (const unlisten of this.moduleUnlistens.values()) {
      unlisten();
    }
    this.moduleUnlistens.clear();
  }

  private async doCreate(
    sessionId: number,
    mode: BuildRunMode,
    useWorktree: boolean,
  ): Promise<BuildRunInstance | null> {
    try {
      const term = new Terminal(TERMINAL_OPTIONS);
      const fitAddon = new FitAddon();
      term.loadAddon(fitAddon);
      // Match modern CLIs' Unicode 11+ glyph widths so emoji output doesn't
      // shear box-drawing borders (xterm defaults to Unicode 6 widths).
      // TERMINAL_OPTIONS sets allowProposedApi, which this addon requires.
      loadUnicode11Widths(term);

      // Per-instance writer (NOT the shared registry writer — see class
      // header comment about key namespace collision).
      const writer = new TerminalWriter();
      writer.register(sessionId, (data) => term.write(data));

      const inst: BuildRunInstance = {
        sessionId,
        mode,
        useWorktree,
        term,
        fitAddon,
        unlisten: null,
        writer,
        opened: false,
        attachedContainer: null,
        resizeObserver: null,
        ptyAlive: false,
      };

      // Wire keystroke + resize handlers for interactive Terminal mode only.
      // Build/Run is one-way output — user input is ignored. Mirrors the
      // pattern in TerminalRegistry.ts for the agent terminal.
      if (mode === 'terminal') {
        term.onData((data) => {
          api.writeToBuildRun(sessionId, data).catch((err) => {
            // The PTY may have exited (e.g. user typed `exit`) — swallow
            // the "not running" error since it's expected.
            if (err !== 'Build run not running') {
              console.error('[BuildRunTerminalRegistry] write_to_build_run failed:', err);
            }
          });
        });
        term.onResize(({ cols, rows }) => {
          api.resizeBuildRun(sessionId, rows, cols).catch(() => {});
        });
      }

      // Subscribe to per-sessionId output events. The event name itself is
      // namespaced by sessionId (e.g. `build-run-output-42`) so we don't
      // need a payload filter — Rust only emits for that node.
      const eventName = `build-run-output-${sessionId}`;
      const unlisten = await listen<string | BuildRunOutputPayload>(eventName, (event) => {
        const data = payloadToBytes(event.payload);
        if (data !== '') writer.append(sessionId, data);
      });
      inst.unlisten = unlisten;

      this.instances.set(instanceKey(sessionId, mode, useWorktree), inst);

      // Install the exit sentinel listener once per sessionId. Marks
      // ptyAlive=false on natural process exit so subsequent
      // writeToBuildRun calls cleanly hit the "Build run not running"
      // path instead of silently vanishing into a dead PTY. The banner
      // is only written if the user is currently viewing this instance
      // (or a sibling mode-conflict variant) — invisible exits stay
      // invisible.
      this.installExitListener(sessionId);

      return inst;
    } catch (e) {
      console.error(`[BuildRunTerminalRegistry] Failed to create terminal for ${sessionId}`, e);
      return null;
    }
  }

  private installExitListener(sessionId: number): void {
    if (this.moduleUnlistens.has(sessionId)) return;
    const eventName = `build-run-exited-${sessionId}`;
    listen<unknown>(eventName, () => {
      // Mark ptyAlive=false on every (mode, useWorktree) variant for this
      // sessionId. There should be at most one after `attach`'s mode-
      // conflict cleanup, but iterate to be safe.
      for (const inst of this.instances.values()) {
        if (inst.sessionId !== sessionId) continue;
        inst.ptyAlive = false;
        if (inst.attachedContainer) {
          inst.term.write('\r\n[process exited]\r\n');
        }
      }
    }).then((unlisten) => {
      this.moduleUnlistens.set(sessionId, unlisten);
    }).catch((err) => {
      // If the Rust side doesn't emit this event (older builds), the
      // listener install silently no-ops. The existing
      // "Build run not running" swallow on writeToBuildRun still keeps
      // the user-visible UX acceptable.
      console.warn(`[BuildRunTerminalRegistry] failed to install exit listener for ${sessionId}:`, err);
    });
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