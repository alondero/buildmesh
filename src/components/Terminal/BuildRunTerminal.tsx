import { useRef } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import * as api from '../../lib/tauri';
import { TERMINAL_OPTIONS } from './terminalConfig';
import { loadUnicode11Widths } from './loadUnicode11Widths';
import { TerminalWriter } from './TerminalWriter';
import { decodeBase64Bytes } from '../../lib/base64';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';

interface BuildRunTerminalProps {
  sessionId: number;
  mode?: 'build' | 'run' | 'terminal';
  useWorktree?: boolean;
  onClose?: () => void;
}

interface BuildRunOutputPayload {
  data: string;
}

function modeLabel(mode: 'build' | 'run' | 'terminal'): string {
  if (mode === 'terminal') return 'Terminal';
  return mode === 'build' ? 'Build' : 'Run';
}

function modeBanner(mode: 'build' | 'run' | 'terminal'): string {
  if (mode === 'terminal') return 'Opening terminal';
  return mode === 'build' ? 'Building' : 'Running';
}

export function BuildRunTerminal({ sessionId, mode = 'build', useWorktree = true, onClose }: BuildRunTerminalProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const termRef = useRef<Terminal | null>(null);
  const fitAddonRef = useRef<FitAddon | null>(null);
  const unlistenRef = useRef<UnlistenFn | null>(null);
  // term.onData / term.onResize return disposables. The xterm teardown in
  // cleanup takes care of them automatically (same pattern as the agent
  // terminal — see TerminalRegistry.ts). We don't track them individually
  // so the cleanup function stays free of any explicit teardown calls.

  useAsyncEffect((signal) => {
    if (!containerRef.current) return;
    // The helper aborts `signal` on cleanup so the late `listen().then`
    // can short-circuit via `if (signal.aborted) unlisten(); return;`
    // before this effect's returned cleanup disposes the resources
    // owned by THIS effect. The xterm's parent TerminalRegistry is
    // not involved here — the term is disposed inline below.

    const term = new Terminal(TERMINAL_OPTIONS);
    const fitAddon = new FitAddon();
    term.loadAddon(fitAddon);
    // Match modern CLIs' Unicode 11+ glyph widths so emoji output doesn't shear
    // box-drawing borders (xterm defaults to Unicode 6 widths). TERMINAL_OPTIONS
    // sets allowProposedApi, which this addon requires. loadUnicode11Widths
    // also patches the small set of BMP emoji the upstream addon ships with
    // the wrong width (notably ⚠ U+26A0) — see loadUnicode11Widths.ts.
    loadUnicode11Widths(term);

    term.open(containerRef.current);
    fitAddon.fit();

    termRef.current = term;
    fitAddonRef.current = fitAddon;

    // Wire keystroke + resize handlers for interactive terminal mode only.
    // Mirrors the agent terminal's pattern in TerminalRegistry.ts:228-282.
    if (mode === 'terminal') {
      term.onData((data) => {
        api.writeToBuildRun(sessionId, data).catch((err) => {
          // The PTY may have exited (e.g. user typed `exit`) — swallow the
          // "not running" error since it's expected. Other errors are real.
          if (err !== 'Build run not running') {
            console.error('[BuildRunTerminal] write_to_build_run failed:', err);
          }
        });
      });
      term.onResize(({ cols, rows }) => {
        api.resizeBuildRun(sessionId, rows, cols).catch(() => {});
      });
    }

    const resizeObserver = new ResizeObserver(() => {
      requestAnimationFrame(() => {
        fitAddonRef.current?.fit();
      });
    });
    resizeObserver.observe(containerRef.current);

    // Coalesce inbound output via the same RAF-batched writer the agent
    // path uses (TerminalRegistry's `writer`). Without this, a verbose
    // build (`cargo build -v`, `tsc --watch`, etc.) turns every compiler
    // line into a separate xterm render pass and pegs CPU. We keep our
    // own writer instance instead of borrowing the registry's because the
    // registry is keyed by node id and the agent terminal has already
    // claimed that slot — sharing would route agent output to this xterm.
    // See issue #303 and tests/unit/build-run-terminal-raf-batching.test.tsx.
    const writer = new TerminalWriter();
    writer.register(sessionId, (data) => term.write(data));

    const eventName = `build-run-output-${sessionId}`;
    listen<string | BuildRunOutputPayload>(eventName, (event) => {
      if (typeof event.payload === 'string') {
        writer.append(sessionId, event.payload);
      } else {
        writer.append(sessionId, decodeBase64Bytes(event.payload.data));
      }
    }).then(unlisten => {
      if (signal.aborted) {
        unlisten();
        return;
      }
      unlistenRef.current = unlisten;
      const bannerPrefix = modeBanner(mode);
      const worktreeSuffix = useWorktree ? ' in worktree' : '';
      term.write(`${bannerPrefix}${worktreeSuffix}...\r\n`);
      api.buildRun(sessionId, mode).catch(err => {
        term.write(`\r\nError: ${String(err)}\r\n`);
      });
    });

    return () => {
      resizeObserver.disconnect();
      unlistenRef.current?.();
      writer.unregister(sessionId);
      term.dispose(); // allow-dispose — BuildRunTerminal is a one-shot panel, not the agent-terminal singleton
      api.closeBuildRun(sessionId).catch(() => {});
    };
  }, [sessionId, mode, useWorktree]);

  return (
    <div className="flex flex-col flex-1 overflow-hidden bg-bg-overlay border-t border-border-default">
      <div className="flex items-center justify-between px-2 py-1 bg-bg-base border-b border-border-default">
        <span className="text-[10px] font-mono text-text-muted">
          {modeLabel(mode)}{useWorktree ? ': worktree' : ''}
        </span>
        <button
          onClick={onClose}
          className="w-4 h-4 flex items-center justify-center rounded text-text-muted hover:text-accent-cyan hover:bg-bg-overlay transition-colors text-[10px]"
          title="Close build/run terminal"
        >
          ×
        </button>
      </div>
      <div
        ref={containerRef}
        className="flex-1 overflow-hidden"
        style={{ padding: '4px' }}
      />
    </div>
  );
}
