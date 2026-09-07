import { useRef } from 'react';
import '@xterm/xterm/css/xterm.css';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import { buildRunTerminalManager } from './BuildRunTerminalRegistry';

interface BuildRunTerminalProps {
  sessionId: number;
  mode?: 'build' | 'run' | 'terminal';
  useWorktree?: boolean;
  focusOnAttach?: boolean;
}

/**
 * Thin React wrapper around `BuildRunTerminalRegistry`.
 *
 * The component no longer owns the xterm Terminal, the PTY lifecycle, or the
 * event listener — those live in the singleton registry so they survive React
 * unmounts. This component is just a DOM host: it attaches the xterm into its
 * container on mount and detaches it on unmount. The card owns explicit tab
 * closing; switching panels preserves the process and scrollback.
 *
 * The detached-on-unmount lifecycle is what fixes the "terminal resets on
 * mesh navigation" bug: when a `NodeCard` unmounts because the user switched
 * meshes, this component's effect cleanup runs `detach` — which removes the
 * xterm element from the DOM but leaves the Terminal object, the scrollback,
 * and the Rust PTY alive. When the user comes back, a fresh `<div>` mounts,
 * the effect runs `attach`, and the same xterm + scrollback is re-parented
 * into the new container without respawning the PTY.
 */
export function BuildRunTerminal({ sessionId, mode = 'build', useWorktree = true, focusOnAttach = true }: BuildRunTerminalProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const focusOnAttachRef = useRef(focusOnAttach);
  focusOnAttachRef.current = focusOnAttach;

  useAsyncEffect((signal) => {
    if (!containerRef.current) return;
    const container = containerRef.current;
    buildRunTerminalManager.attach(sessionId, mode, useWorktree, container, signal).then(instance => {
      if (!signal.aborted && focusOnAttachRef.current) instance?.term.focus();
    });
    return () => {
      // DOM-only teardown — the registry preserves the xterm + PTY. The
      // next mount's `attach` will re-parent the existing `.xterm` element
      // into the new container with zero re-initialization. Always run
      // `detach` unconditionally — `useAsyncEffect` aborts the signal
      // BEFORE invoking this cleanup, so a `signal.aborted` guard here
      // would skip `detach` entirely (bug seen in code review).
      buildRunTerminalManager.detach(sessionId, mode, useWorktree);
    };
  }, [sessionId, mode, useWorktree]);

  return (
    <div ref={containerRef} className="min-h-0 flex-1 overflow-hidden bg-bg-overlay p-1" />
  );
}
