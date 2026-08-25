/**
 * CircuitsProbeTab — the Probe Panel's Autopilot Circuits tab
 * (spec #1205).
 *
 * Milestone-4 surface (issue #1209): authoring moved into the full-screen
 * React Flow canvas editor. "New Circuit" creates the canonical
 * server-side skeleton and immediately opens the editor over the center
 * workspace; every row gets an "Edit Flow" button. The tab itself keeps
 * the circuit list, enable toggle, Trigger Now, delete, and the per-
 * circuit run ledger.
 *
 * Live updates ride the backend's `circuit-run-updated` event so a run
 * visibly lands as Completed without a manual refresh; every user
 * action refetches anyway.
 */

import { useCallback, useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { formatError } from '../../lib/errorUtils';
import {
  approveCircuitStep,
  createCircuit,
  deleteCircuit,
  listCircuitsWithRuns,
  pauseCircuitRun,
  resumeCircuitRun,
  setCircuitEnabled,
  triggerCircuitNow,
  type CircuitTriggerKind,
  type CircuitWithRuns,
} from '../../lib/tauri';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useUIStore } from '../../stores/uiStore';
import { EmptyState } from '../shared/Spinner';

/** Tailwind token classes for the ledger's run/step status vocabulary. */
function statusClass(status: string): string {
  switch (status) {
    case 'completed':
      return 'text-status-success';
    case 'running':
      return 'text-accent-cyan animate-pulse';
    case 'paused':
    case 'blocked':
      return 'text-status-warning';
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
  const openCircuitEditor = useUIStore((s) => s.openCircuitEditor);
  const [rows, setRows] = useState<CircuitWithRuns[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  // New-Circuit row: name + trigger shape, then straight into the editor.
  const [newName, setNewName] = useState('');
  const [triggerKind, setTriggerKind] = useState<CircuitTriggerKind>('manual');
  const [triggerLabel, setTriggerLabel] = useState('');
  const [intervalSeconds, setIntervalSeconds] = useState(300);
  const needsLabel = triggerKind === 'github_issue_label' || triggerKind === 'github_pr_label';

  const load = useCallback(async () => {
    if (activeMeshId === null) {
      setRows([]);
      return;
    }
    try {
      // One batched IPC: circuits with their recent run ledgers.
      const rows = await listCircuitsWithRuns(activeMeshId, 10);
      setRows(rows);
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
      const circuit = await createCircuit(
        activeMeshId!,
        name,
        '',
        1,
        '', // the prompt is authored in the canvas editor's inspector now
        triggerKind,
        triggerKind === 'manual' ? undefined : triggerLabel.trim(),
        triggerKind === 'interval' ? intervalSeconds : undefined
      );
      setNewName('');
      setTriggerLabel('');
      openCircuitEditor(circuit.id);
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
      {/* New Circuit row — authoring itself happens in the canvas editor. */}
      <div className="px-3 py-2 border-b border-border-subtle shrink-0">
        <div className="flex items-center gap-1 mb-1">
          <input
            value={newName}
            onChange={(e) => setNewName(e.target.value)}
            placeholder="New circuit name"
            aria-label="New circuit name"
            data-testid="circuit-name-input"
            className="flex-1 min-w-0 px-2 py-1 bg-bg-surface border border-border-subtle rounded-md text-text-primary focus:outline-none"
          />
          <button
            type="button"
            onClick={handleCreate}
            disabled={
              busy ||
              newName.trim() === '' ||
              (needsLabel && triggerLabel.trim() === '')
            }
            data-testid="circuit-create-button"
            className="px-2 py-1 rounded-md bg-accent-cyan/15 text-accent-cyan hover:bg-accent-cyan/25 disabled:opacity-40 shrink-0"
          >
            New Circuit
          </button>
        </div>
        <div className="flex items-center gap-1">
          <select
            value={triggerKind}
            onChange={(e) => setTriggerKind(e.target.value as CircuitTriggerKind)}
            aria-label="Circuit trigger"
            data-testid="circuit-trigger-select"
            className="px-1.5 py-0.5 bg-bg-surface border border-border-subtle rounded-md text-xs text-text-primary focus:outline-none"
          >
            <option value="manual">Manual</option>
            <option value="interval">Interval</option>
            <option value="github_issue_label">Issue label</option>
            <option value="github_pr_label">PR label</option>
          </select>
          {needsLabel && (
            <input
              value={triggerLabel}
              onChange={(e) => setTriggerLabel(e.target.value)}
              placeholder="Label (e.g. buildmesh:run)"
              aria-label="Trigger label"
              data-testid="circuit-trigger-label-input"
              className="flex-1 min-w-0 px-2 py-0.5 bg-bg-surface border border-border-subtle rounded-md text-xs text-text-primary focus:outline-none"
            />
          )}
          {triggerKind === 'interval' && (
            <label className="flex items-center gap-1 text-xs text-text-muted shrink-0">
              every
              <input
                type="number"
                min={60}
                value={intervalSeconds}
                onChange={(e) => setIntervalSeconds(Number(e.target.value) || 300)}
                aria-label="Interval seconds"
                data-testid="circuit-interval-input"
                className="w-16 px-1.5 py-0.5 bg-bg-surface border border-border-subtle rounded-md text-xs text-text-primary focus:outline-none"
              />
              s
            </label>
          )}
        </div>
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
            hint="Create one above — it opens straight in the flow editor."
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
                    onClick={() => openCircuitEditor(circuit.id)}
                    data-testid={`circuit-edit-flow-${circuit.id}`}
                    className="px-2 py-0.5 rounded-md bg-accent-violet/15 text-accent-violet hover:bg-accent-violet/25"
                  >
                    Edit Flow
                  </button>
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
                    <li key={run.id} className="flex items-baseline gap-2 py-0.5 flex-wrap">
                      <span className="text-text-muted">#{run.id}</span>
                      <span className={statusClass(run.state)} data-testid={`run-state-${run.id}`}>
                        {run.state}
                      </span>
                      {/* Graceful pause/resume (#1207). */}
                      {run.state === 'running' && (
                        <button
                          type="button"
                          onClick={() => runAction(() => pauseCircuitRun(run.id))}
                          disabled={busy}
                          data-testid={`run-pause-${run.id}`}
                          className="px-1.5 rounded-md bg-text-muted/10 text-text-muted hover:text-text-primary"
                        >
                          Pause
                        </button>
                      )}
                      {run.state === 'paused' && (
                        <button
                          type="button"
                          onClick={() => runAction(() => resumeCircuitRun(run.id))}
                          disabled={busy}
                          data-testid={`run-resume-${run.id}`}
                          className="px-1.5 rounded-md bg-accent-cyan/15 text-accent-cyan hover:bg-accent-cyan/25"
                        >
                          Resume
                        </button>
                      )}
                      <span className="truncate text-text-muted" title={run.trigger_identity}>
                        {steps.length > 0
                          ? steps.map((s) => `${s.node_id}:${s.status}`).join(' → ')
                          : 'no steps'}
                      </span>
                      {/* Blocked collaborator gates (#1207): amber badge + Approve. */}
                      {steps
                        .filter((s) => s.status === 'blocked')
                        .map((s) => (
                          <span
                            key={s.node_id}
                            className="inline-flex items-center gap-1 px-1.5 rounded-md bg-status-warning/15 text-status-warning"
                            data-testid={`blocked-badge-${run.id}-${s.node_id}`}
                          >
                            ⏸ waiting for approval: {s.node_id}
                            <button
                              type="button"
                              onClick={() => runAction(() => approveCircuitStep(run.id, s.node_id))}
                              disabled={busy}
                              aria-label={`Approve ${s.node_id} on run ${run.id}`}
                              data-testid={`approve-${run.id}-${s.node_id}`}
                              className="px-1 rounded-md bg-status-warning/25 hover:bg-status-warning/40 font-semibold"
                            >
                              Approve
                            </button>
                          </span>
                        ))}
                      {/* Failure detail — first errored step surfaces its message. */}
                      {(() => {
                        const failed = steps.find((s) => s.error_message);
                        return failed ? (
                          <span
                            className="text-status-error truncate"
                            data-testid={`run-error-${run.id}`}
                            title={failed.error_message ?? ''}
                          >
                            ⚠ {failed.error_message}
                          </span>
                        ) : null;
                      })()}
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
