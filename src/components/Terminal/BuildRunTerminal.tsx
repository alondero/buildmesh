import { useRef, useCallback } from 'react';
import '@xterm/xterm/css/xterm.css';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { buildRunTerminalManager } from './BuildRunTerminalRegistry';

interface BuildRunTerminalProps {
  sessionId: number;
  mode?: 'build' | 'run' | 'terminal';
  useWorktree?: boolean;
  onClose?: () => void;
}

function modeLabel(mode: 'build' | 'run' | 'terminal'): string {
  if (mode === 'terminal') return 'Terminal';
  return mode === 'build' ? 'Build' : 'Run';
}

/**
 * Thin React wrapper around `BuildRunTerminalRegistry`.
 *
 * The component no longer owns the xterm Terminal, the PTY lifecycle, or the
 * event listener — those live in the singleton registry so they survive React
 * unmounts. This component is just a DOM host: it attaches the xterm into its
 * container on mount, detaches it on unmount, and disposes the whole session
 * (xterm + PTY) only when the user explicitly clicks the X.
 *
 * The detached-on-unmount lifecycle is what fixes the "terminal resets on
 * mesh navigation" bug: when a `NodeCard` unmounts because the user switched
 * meshes, this component's effect cleanup runs `detach` — which removes the
 * xterm element from the DOM but leaves the Terminal object, the scrollback,
 * and the Rust PTY alive. When the user comes back, a fresh `<div>` mounts,
 * the effect runs `attach`, and the same xterm + scrollback is re-parented
 * into the new container without respawning the PTY.
 */
export function BuildRunTerminal({ sessionId, mode = 'build', useWorktree = true, onClose }: BuildRunTerminalProps) {
  const containerRef = useRef<HTMLDivElement>(null);

  useAsyncEffect((signal) => {
    if (!containerRef.current) return;
    const container = containerRef.current;
    buildRunTerminalManager.attach(sessionId, mode, useWorktree, container);
    return () => {
      // DOM-only teardown — the registry preserves the xterm + PTY. The
      // next mount's `attach` will re-parent the existing `.xterm` element
      // into the new container with zero re-initialization.
      if (!signal.aborted) {
        buildRunTerminalManager.detach(sessionId, mode, useWorktree);
      }
    };
  }, [sessionId, mode, useWorktree]);

  // X button: full teardown (kills PTY + disposes xterm + clears parent state).
  // We must run `dispose` BEFORE `onClose` so the PTY is killed before the
  // parent's `setBuildRunOpen(null)` causes this component to unmount —
  // otherwise the React effect cleanup's `detach` would race with our intent
  // to close. Order matters.
  const handleClose = useCallback(() => {
    buildRunTerminalManager.dispose(sessionId, mode, useWorktree); // allow-dispose — explicit X-button close; the equivalent of "node deleted" for the agent terminal
    onClose?.();
  }, [sessionId, mode, useWorktree, onClose]);

  return (
    <div className="flex flex-col flex-1 overflow-hidden bg-bg-overlay border-t border-border-default">
      <div className="flex items-center justify-between px-2 py-1 bg-bg-base border-b border-border-default">
        <span className="text-[10px] font-mono text-text-muted">
          {modeLabel(mode)}{useWorktree ? ': worktree' : ''}
        </span>
        <button
          onClick={handleClose}
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