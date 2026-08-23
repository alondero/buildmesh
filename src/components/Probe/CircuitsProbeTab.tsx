/**
 * CircuitsProbeTab — the Probe Panel's Autopilot Circuits tab
 * (spec #1205 / walking skeleton #1206).
 *
 * Milestone-1 surface: a throwaway authoring form (name + prompt → the
 * canonical server-side Manual → SpawnAgentNode → Notify blueprint), a
 * circuit list with enable toggle + Trigger Now + delete, and a per-
 * circuit run list fed by the `autopilot_circuit_runs` /
 * `_run_steps` ledger. The real React Flow canvas editor arrives in a
 * later milestone; this tab only needs to prove the loop end-to-end.
 *
 * Live updates ride the backend's `circuit-run-updated` event so a run
 * visibly lands as Completed without a manual refresh; every user
 * action refetches anyway.
 */

import { useCallback, useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { formatError } from '../../lib/errorUtils';
import {
  createCircuit,
  deleteCircuit,
  listCircuitRuns,
  listCircuits,
  setCircuitEnabled,
  triggerCircuitNow,
  type AutopilotCircuit,
  type CircuitRunDetail,
} from '../../lib/tauri';
import { useProbeContext } from '../../hooks/useProbeContext';
import { EmptyState } from '../shared/Spinner';

interface CircuitWithRuns {
  circuit: AutopilotCircuit;
  runs: CircuitRunDetail[];
}

/** Tailwind token classes for the ledger's run/step status vocabulary. */
export function statusClass(status: string): string {
  switch (status) {
    case 'completed':
      return 'text-status-success';
    case 'running':
      return 'text-accent-cyan animate-pulse';
    case 'pending':
    case 'pending_slot':
      return 'text-text-muted';
    case 'failed':
    case 'cancelled':
      return 'text-status-error';
    default:
      return 'text-text-muted';
  }
}

export function CircuitsProbeTab() {
  const { activeMeshId } = useProbeContext();
  const [rows, setRows] = useState<CircuitWithRuns[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // Create-form state (throwaway authoring until the canvas editor).
  const [newName, setNewName] = useState('');
  const [newPrompt, setNewPrompt] = useState('');

  const load = useCallback(async () => {
    if (activeMeshId === null) {
      setRows([]);
      return;
    }
    try {
      const circuits = await listCircuits(activeMeshId);
      const withRuns = await Promise.all(
        circuits.map(async (circuit) => ({
          circuit,
          runs: await listCircuitRuns(circuit.id, 10).catch(() => []),
        })),
      );
      setRows(withRuns);
      setLoadError(null);
    } catch (err) {
      console.error('Failed to load circuits:', err);
      setLoadError(formatError(err));
    }
  }, [activeMeshId]);

  useEffect(() => {
    void load();
  }, [load]);

  // The worker emits `circuit-run-updated` whenever a run changes state;
  // reload so a finishing run flips to Completed in place.
  useEffect(() => {
    if (activeMeshId === null) return;
    let disposed = false;
    const unlisten = listen('circuit-run-updated', () => {
      if (!disposed) void load();
    });
    return () => {
      disposed = true;
      void unlisten.then((fn) => fn());
    };
  }, [activeMeshId, load]);

  const runAction = async (fn: () => Promise<unknown>) => {
    setBusy(true);
    setActionError(null);
    try {
      await fn();
      await load();
    } catch (err) {
      console.error('Circuit action failed:', err);
      setActionError(formatError(err));
    } finally {
      setBusy(false);
    }
  };

  const handleCreate = () =>
    runAction(async () => {
      const name = newName.trim();
      if (name === '') return;
      await createCircuit(activeMeshId!, name, '', 1, newPrompt.trim());
      setNewName('');
      setNewPrompt('');
    });

  if (activeMeshId === null) {
    return (
      <div className="p-4">
        <EmptyState label="No mesh selected" hint="Select a mesh to manage its Autopilot Circuits." />
      </div>
    );
  }

  return (
    <div className="flex flex-col h-full overflow-y-auto text-sm" data-testid="circuits-probe-tab">
      {/* Create form */}
      <div className="px-3 py-2 border-b border-border-subtle shrink-0">
        <input
          value={newName}
          onChange={(e) => setNewName(e.target.value)}
          placeholder="New circuit name"
          aria-label="New circuit name"
          data-testid="circuit-name-input"
          className="w-full mb-1 px-2 py-1 bg-bg-surface border border-border-subtle rounded-md text-text-primary focus:outline-none"
        />
        <textarea
          value={newPrompt}
          onChange={(e) => setNewPrompt(e.target.value)}
          placeholder="Initial agent prompt…"
          aria-label="Initial agent prompt"
          data-testid="circuit-prompt-input"
          rows={3}
          className="w-full mb-1 px-2 py-1 bg-bg-surface border border-border-subtle rounded-md text-text-primary font-mono text-xs resize-none focus:outline-none"
        />
        <button
          type="button"
          onClick={handleCreate}
          disabled={busy || newName.trim() === ''}
          data-testid="circuit-create-button"
          className="px-2 py-0.5 rounded-md bg-accent-cyan/15 text-accent-cyan hover:bg-accent-cyan/25 disabled:opacity-40"
        >
          Create Circuit
        </button>
      </div>

      {(loadError !== null || actionError !== null) && (
        <div className="px-3 py-1 text-xs text-status-error" role="alert">
          {actionError ?? loadError}
        </div>
      )}

      {rows.length === 0 ? (
        <div className="p-4">
          <EmptyState
            label="No circuits yet"
            hint="Create one above, then hit Trigger Now."
          />
        </div>
      ) : (
        <ul className="flex flex-col gap-1 p-2">
          {rows.map(({ circuit, runs }) => (
            <li key={circuit.id} className="rounded-md border border-border-subtle p-2" data-testid="circuit-row">
              <div className="flex items-center justify-between gap-2">
                <label className="flex items-center gap-1.5 min-w-0">
                  <input
                    type="checkbox"
                    checked={circuit.enabled}
                    onChange={() =>
                      runAction(() => setCircuitEnabled(circuit.id, !circuit.enabled))
                    }
                    aria-label={`Enable ${circuit.name}`}
                    data-testid={`circuit-enabled-${circuit.id}`}
                  />
                  <span className="truncate text-text-primary">{circuit.name}</span>
                </label>
                <span className="flex items-center gap-1 shrink-0">
                  <button
                    type="button"
                    onClick={() => runAction(() => triggerCircuitNow(circuit.id))}
                    disabled={busy || !circuit.enabled}
                    data-testid={`circuit-trigger-${circuit.id}`}
                    className="px-2 py-0.5 rounded-md bg-accent-cyan/15 text-accent-cyan hover:bg-accent-cyan/25 disabled:opacity-40"
                  >
                    Trigger Now
                  </button>
                  <button
                    type="button"
                    onClick={() => runAction(() => deleteCircuit(circuit.id))}
                    disabled={busy}
                    aria-label={`Delete ${circuit.name}`}
                    data-testid={`circuit-delete-${circuit.id}`}
                    className="px-1.5 py-0.5 rounded-md text-text-muted hover:text-status-error"
                  >
                    ✕
                  </button>
                </span>
              </div>

              {/* Run list */}
              {runs.length > 0 && (
                <ul className="mt-1 ml-5 text-xs" data-testid={`circuit-runs-${circuit.id}`}>
                  {runs.map(({ run, steps }) => (
                    <li key={run.id} className="flex items-baseline gap-2 py-0.5">
                      <span className="text-text-muted">#{run.id}</span>
                      <span className={statusClass(run.state)} data-testid={`run-state-${run.id}`}>
                        {run.state}
                      </span>
                      <span className="truncate text-text-muted" title={run.trigger_identity}>
                        {steps.length > 0
                          ? steps.map((s) => `${s.node_id}:${s.status}`).join(' → ')
                          : 'no steps'}
                      </span>
                    </li>
                  ))}
                </ul>
              )}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}
