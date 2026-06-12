import { useEffect, useRef } from 'react';
import { Terminal } from '@xterm/xterm';
import { FitAddon } from '@xterm/addon-fit';
import '@xterm/xterm/css/xterm.css';
import { listen, UnlistenFn } from '@tauri-apps/api/event';
import { invoke } from '@tauri-apps/api/core';
import { TERMINAL_OPTIONS } from './terminalConfig';
import { loadUnicode11Widths } from './loadUnicode11Widths';

interface BuildRunTerminalProps {
  sessionId: number;
  mode?: 'build' | 'run' | 'terminal';
  useWorktree?: boolean;
  onClose?: () => void;
}

interface BuildRunOutputPayload {
  data: string;
}

function decodeBase64Bytes(data: string): Uint8Array {
  const binary = globalThis.atob(data);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) {
    bytes[i] = binary.charCodeAt(i);
  }
  return bytes;
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

  useEffect(() => {
    if (!containerRef.current) return;
    let cancelled = false;

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
        invoke('write_to_build_run', { nodeId: sessionId, data }).catch((err) => {
          // The PTY may have exited (e.g. user typed `exit`) — swallow the
          // "not running" error since it's expected. Other errors are real.
          if (err !== 'Build run not running') {
            console.error('[BuildRunTerminal] write_to_build_run failed:', err);
          }
        });
      });
      term.onResize(({ cols, rows }) => {
        invoke('resize_build_run', { nodeId: sessionId, rows, cols }).catch(() => {});
      });
    }

    const resizeObserver = new ResizeObserver(() => {
      requestAnimationFrame(() => {
        fitAddonRef.current?.fit();
      });
    });
    resizeObserver.observe(containerRef.current);

    const eventName = `build-run-output-${sessionId}`;
    listen<string | BuildRunOutputPayload>(eventName, (event) => {
      if (typeof event.payload === 'string') {
        term.write(event.payload);
      } else {
        term.write(decodeBase64Bytes(event.payload.data));
      }
    }).then(unlisten => {
      if (cancelled) {
        unlisten();
        return;
      }
      unlistenRef.current = unlisten;
      const bannerPrefix = modeBanner(mode);
      const worktreeSuffix = useWorktree ? ' in worktree' : '';
      term.write(`${bannerPrefix}${worktreeSuffix}...\r\n`);
      invoke('build_run', { nodeId: sessionId, mode }).catch(err => {
        term.write(`\r\nError: ${String(err)}\r\n`);
      });
    });

    return () => {
      cancelled = true;
      resizeObserver.disconnect();
      unlistenRef.current?.();
      term.dispose(); // allow-dispose — BuildRunTerminal is a one-shot panel, not the agent-terminal singleton
      invoke('close_build_run', { nodeId: sessionId }).catch(() => {});
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
