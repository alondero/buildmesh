/**
 * CircuitEditorOverlay — the full-screen canvas editor shell (issue
 * #1209).
 *
 * Mounted by `AgentNodeView` next to the diff overlay whenever the uiStore's
 * `activeCircuitEditorId` is set. Same overlay discipline as
 * `CenterDiffOverlay`: the editor only *covers* the center workspace — the
 * terminal grid behind it stays mounted and every PTY keeps running via
 * the TerminalManager singleton, so closing returns to the exact grid.
 *
 * Owns the data plumbing (circuit row + run ledgers + live
 * `circuit-run-updated` refetch) so the inner `CircuitFlowEditor` can stay
 * a pure function of its props.
 */

import { useCallback, useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { formatError } from '../../lib/errorUtils';
import {
  getCircuit,
  listCircuitRuns,
  type AutopilotCircuit,
  type CircuitRunDetail,
} from '../../lib/tauri';
import { useUIStore } from '../../stores/uiStore';
import { CircuitFlowEditor } from './CircuitFlowEditor';
import { LoadingState } from '../shared/Spinner';

export function CircuitEditorOverlay() {
  const circuitId = useUIStore((s) => s.activeCircuitEditorId);
  const closeCircuitEditor = useUIStore((s) => s.closeCircuitEditor);

  if (circuitId === null) return null;
  return (
    <LoadedCircuitEditor
      key={circuitId}
      circuitId={circuitId}
      onClose={closeCircuitEditor}
    />
  );
}

function LoadedCircuitEditor({
  circuitId,
  onClose,
}: {
  circuitId: number;
  onClose: () => void;
}) {
  const [circuit, setCircuit] = useState<AutopilotCircuit | null>(null);
  const [runs, setRuns] = useState<CircuitRunDetail[]>([]);
  const [error, setError] = useState<string | null>(null);
  // Bumped after saves so a stale graph_json refetches.
  const [version, setVersion] = useState(0);

  const load = useCallback(async () => {
    try {
      const [circuit, runs] = await Promise.all([
        getCircuit(circuitId),
        listCircuitRuns(circuitId, 30),
      ]);
      setCircuit(circuit);
      setRuns(runs);
      setError(null);
    } catch (err) {
      console.error('Failed to load circuit for editing:', err);
      setError(formatError(err));
    }
  }, [circuitId]);

  useEffect(() => {
    void load();
  }, [load, version]);

  // Live runs: the worker emits on every state change; refetch so node
  // overlays pulse/check/alert without leaving the canvas.
  useEffect(() => {
    let disposed = false;
    const unlisten = listen('circuit-run-updated', () => {
      if (!disposed) void load();
    });
    return () => {
      disposed = true;
      void unlisten.then((fn) => fn());
    };
  }, [load]);

  if (error !== null) {
    return (
      <div className="absolute inset-0 z-40 flex flex-col items-center justify-center gap-2 bg-bg-base" data-testid="circuit-editor-overlay">
        <p className="text-sm text-status-error">{error}</p>
        <button type="button" onClick={onClose} className="px-2 py-1 rounded-md bg-accent-cyan/15 text-accent-cyan text-xs">
          Back to Terminals
        </button>
      </div>
    );
  }

  if (circuit === null) {
    return (
      <div className="absolute inset-0 z-40 bg-bg-base" data-testid="circuit-editor-overlay">
        <LoadingState label="Loading circuit…" />
      </div>
    );
  }

  return (
    <div className="absolute inset-0 z-40" data-testid="circuit-editor-overlay">
      <CircuitFlowEditor
        circuit={circuit}
        runs={runs}
        onClose={onClose}
        onSaved={() => setVersion((v) => v + 1)}
      />
    </div>
  );
}
