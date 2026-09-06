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
 *
 * Scroll ownership (issue #1468)
 * ------------------------------
 * ONE scroll owner: the `[data-testid=circuits-probe-body]` div. The tab
 * root is layout-only (`flex flex-col h-full min-h-0`), which keeps
 * `ProbePanel`'s outer `flex-1 overflow-y-auto` inert — its content is
 * exactly its own height, so it never gains a scrollbar of its own. The
 * View navigation sits OUTSIDE the scroller as a `shrink-0` sibling,
 * so it stays put while the ledger scrolls. This is the same shape the
 * shared `<ProbeTabBody>` primitive gives every other tab; Circuits
 * predates it and previously put `overflow-y-auto` on its own root,
 * stacking a second scroller inside the panel's.
 *
 * `overflow-x-hidden` is explicit, not decorative: `overflow-y-auto`
 * alone computes `overflow-x: auto` (CSS forbids one axis being `visible`
 * while the other scrolls), which is how a long diagnostic used to be
 * able to scroll the whole tab sideways.
 */

import { useCallback, useEffect, useState } from 'react';
import { listen } from '@tauri-apps/api/event';
import { formatError } from '../../lib/errorUtils';
import {
  approveCircuitStep,
  cancelCircuitRun,
  createCircuit,
  deleteCircuit,
  listCircuitProbe,
  moveCircuitRun,
  pauseCircuitRun,
  resumeCircuitRun,
  setCircuitEnabled,
  triggerCircuitNow,
  type CircuitBlueprintKind,
  type CircuitQueueEntry,
  type CircuitTriggerKind,
  type CircuitWithRuns,
} from '../../lib/tauri';
import { isTerminalRunState } from '../Circuits/circuitGraphModel';
import { countActiveRuns, countRunningSteps, pendingAdmissionDetail } from '../Circuits/runDiagnostics';
import { useProbeContext } from '../../hooks/useProbeContext';
import { useUIStore } from '../../stores/uiStore';
import { useMeshStore } from '../../stores/meshStore';
import { EmptyState } from '../shared/Spinner';
import { CircuitRunCard } from './CircuitRunCard';

export function CircuitsProbeTab() {
  const { activeMeshId } = useProbeContext();
  const openCircuitEditor = useUIStore((s) => s.openCircuitEditor);
  // `meshesById` already carries every column after #1467 / #1470 — including
  // `circuit_run_capacity` — so no new IPC is needed to surface the run-
  // admission budget to the Probe (#1475).
  const meshRow = useMeshStore((s) =>
    activeMeshId === null ? undefined : s.meshesById.get(activeMeshId)
  );
  const meshRunCapacity = meshRow?.circuit_run_capacity ?? 0;
  const [rows, setRows] = useState<CircuitWithRuns[]>([]);
  const [queue, setQueue] = useState<CircuitQueueEntry[]>([]);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [view, setView] = useState<'activity' | 'history' | 'queue' | 'manage'>('activity');
  const [confirmDeleteCircuitId, setConfirmDeleteCircuitId] = useState<number | null>(null);
  // Mesh-level run count, computed from every circuit's ledger — mirrors
  // `db::count_active_circuit_runs` (#1467). Computed once per render and
  // shared between the queue admission hint and every run card's
  // `CircuitCapacity` (issue #1475 — the same number feeds both surfaces
  // so the wording cannot disagree).
  const meshActiveRuns = countActiveRuns(rows.flatMap(({ runs }) => runs));
  /**
   * Explicit run-card disclosure overrides, keyed by run id. Absent means
   * "use the default" (all diagnostics closed) — storing
   * only deliberate toggles is what lets the `circuit-run-updated`
   * refetch land without snapping a card the user just opened shut.
   */
  const [runExpandOverrides, setRunExpandOverrides] = useState<Record<number, boolean>>({});
  /**
   * Clock the duration labels measure a live run against.
   *
   * This has to tick on its own. Baselining it on fetch was wrong: a run's
   * `updated_at` only moves on a *state transition*, so a step that churns
   * for ten minutes emits no `circuit-run-updated` and the elapsed time
   * would sit frozen at whatever it read when the tab last loaded — which
   * is worse than showing nothing, because a stalled run would look fresh.
   * `UsageTab` sets the precedent for a 1s tick on a relative-time label.
   */
  const [now, setNow] = useState(() => new Date());
  // New-Circuit row: name + trigger shape, then straight into the editor.
  const [newName, setNewName] = useState('');
  const [blueprint, setBlueprint] = useState<CircuitBlueprintKind>('walking_skeleton');
  const [triggerKind, setTriggerKind] = useState<CircuitTriggerKind>('manual');
  const [triggerLabel, setTriggerLabel] = useState('');
  const [intervalSeconds, setIntervalSeconds] = useState(300);
  const isReviewBlueprint = blueprint === 'issue_driven_autopilot_review';
  const effectiveTriggerKind: CircuitTriggerKind = isReviewBlueprint
    ? 'github_issue_label'
    : triggerKind;
  const needsLabel =
    effectiveTriggerKind === 'github_issue_label' || effectiveTriggerKind === 'github_pr_label';

  const load = useCallback(async () => {
    if (activeMeshId === null) {
      setRows([]);
      setQueue([]);
      return;
    }
    try {
      // Ledger cards and the complete queue hydrate through one IPC payload.
      const snapshot = await listCircuitProbe(activeMeshId, 10);
      setRows(snapshot.circuits);
      setQueue(snapshot.queue);
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

  // Advance the duration clock while any visible run is still open. Gated
  // on `hasLiveRun` so a tab showing only finished runs — whose durations
  // are fixed by their own `updated_at` — re-renders never.
  const hasLiveRun = rows.some(({ runs }) =>
    runs.some(({ run }) => !isTerminalRunState(run.state))
  );
  useEffect(() => {
    if (!hasLiveRun) return;
    const id = setInterval(() => setNow(new Date()), 1000);
    return () => clearInterval(id);
  }, [hasLiveRun]);

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

  // Keep deliberate disclosures across live refreshes and view changes.
  const toggleRunExpanded = (runId: number) => {
    setRunExpandOverrides((prev) => {
      const current = prev[runId] ?? false;
      return { ...prev, [runId]: !current };
    });
  };

  const handleCreate = () =>
    runAction(async () => {
      const name = newName.trim();
      if (name === '') return;
      const circuit = await createCircuit(
        activeMeshId!,
        name,
        '',
        isReviewBlueprint ? 2 : 1,
        '', // the prompt is authored in the canvas editor's inspector now
        effectiveTriggerKind,
        effectiveTriggerKind === 'manual' ? undefined : triggerLabel.trim(),
        effectiveTriggerKind === 'interval' ? intervalSeconds : undefined,
        blueprint
      );
      setNewName('');
      setTriggerLabel('');
      setBlueprint('walking_skeleton');
      openCircuitEditor(circuit.id);
    });

  const needsAttention = (detail: CircuitWithRuns['runs'][number]) =>
    detail.run.state === 'failed' || detail.run.state === 'paused' ||
    detail.steps.some((step) => step.status === 'blocked');
  // A later finished run supersedes an old failure; History retains
  // every result. Only the latest finished run can flag circuit attention.
  const latestFinishedIds = new Set(rows.map(({ runs }) =>
    Math.max(-1, ...runs.filter(({ run }) => isTerminalRunState(run.state)).map(({ run }) => run.id))));
  const isActivity = (detail: CircuitWithRuns['runs'][number]) =>
    detail.run.state !== 'pending' &&
    (!isTerminalRunState(detail.run.state) ||
      (detail.run.state === 'failed' && latestFinishedIds.has(detail.run.id)));
  const activityCount = rows.flatMap(({ runs }) => runs).filter(isActivity).length;
  const attentionCount = rows.flatMap(({ runs }) => runs).filter(isActivity).filter(needsAttention).length;
  const visibleRows = rows.map((row) => ({
    ...row,
    visibleRuns: row.runs.filter((detail) => view === 'history'
      ? isTerminalRunState(detail.run.state)
      : isActivity(detail)).sort((a, b) =>
        (view === 'activity' ? Number(needsAttention(b)) - Number(needsAttention(a)) : 0) ||
        b.run.id - a.run.id),
  })).filter((row) => view === 'manage' || row.visibleRuns.length > 0)
    .sort((a, b) => view === 'activity'
      ? Number(b.visibleRuns.some(needsAttention)) - Number(a.visibleRuns.some(needsAttention))
      : 0);

  if (activeMeshId === null) {
    return (
      <div className="p-4">
        <EmptyState label="No mesh selected" hint="Select a mesh to manage its Autopilot Circuits." />
      </div>
    );
  }

  return (
    // Layout only — see the "Scroll ownership" note in the file header.
    <div className="flex flex-col h-full min-h-0 text-sm" data-testid="circuits-probe-tab">
      <div className="px-3 py-2 border-b border-border-subtle shrink-0">
        <p className="text-xs text-text-secondary mb-2" role="status">
          {attentionCount > 0 ? `${attentionCount} need attention` : activityCount > 0 ? 'Circuits are working' : 'No active runs'}
        </p>
        <div className="grid grid-cols-2 gap-1" aria-label="Circuit views">
          {(['activity', 'history', 'queue', 'manage'] as const).map((item) => (
            <button key={item} type="button" aria-pressed={view === item}
              onClick={() => setView(item)} data-testid={`circuits-view-${item}`}
              className={`rounded-md px-2 py-1 text-xs focus-visible:ring-1 focus-visible:ring-accent-cyan ${view === item ? 'bg-accent-cyan/15 text-accent-cyan' : 'text-text-muted hover:bg-bg-card-hover'}`}>
              {item === 'activity' ? `Activity (${activityCount})` : item === 'queue' ? `Queue (${queue.length})` : item === 'history' ? 'History' : 'Manage'}
            </button>
          ))}
        </div>
      </div>
      {(loadError !== null || actionError !== null) && (
        <div className="px-3 py-1 text-xs text-status-error shrink-0 break-words" role="alert">
          {actionError ?? loadError}
        </div>
      )}
      <div className="flex-1 min-h-0 overflow-y-auto overflow-x-hidden" data-testid="circuits-probe-body">
      {/* Configuration scrolls with Manage, keeping navigation reachable
          even when the form is taller than a short dock. */}
      {view === 'manage' && <div className="px-3 py-2 border-b border-border-subtle">
        <h3 className="text-xs font-semibold text-text-primary mb-2">Create a circuit</h3>
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
        <div className="flex flex-col items-stretch gap-2 min-w-0 [&_select]:w-full [&_select]:min-w-0">
          <select
            value={blueprint}
            onChange={(e) => {
              const next = e.target.value as CircuitBlueprintKind;
              setBlueprint(next);
              if (next === 'issue_driven_autopilot_review') {
                setTriggerKind('github_issue_label');
              }
            }}
            aria-label="Circuit blueprint"
            data-testid="circuit-blueprint-select"
            className="px-1.5 py-0.5 bg-bg-surface border border-border-subtle rounded-md text-xs text-text-primary focus:outline-none"
          >
            <option value="walking_skeleton">Walking skeleton</option>
            <option value="issue_driven_autopilot_review">Issue-driven Autopilot + PR review</option>
          </select>
          <select
            value={effectiveTriggerKind}
            onChange={(e) => setTriggerKind(e.target.value as CircuitTriggerKind)}
            aria-label="Circuit trigger"
            data-testid="circuit-trigger-select"
            disabled={isReviewBlueprint}
            className="px-1.5 py-0.5 bg-bg-surface border border-border-subtle rounded-md text-xs text-text-primary focus:outline-none"
          >
            <option value="manual">Manual</option>
            <option value="interval">Interval</option>
            <option value="github_issue_label">Issue label</option>
            <option value="github_pr_label">PR label</option>
          </select>
          {isReviewBlueprint && (
            <span className="text-xs text-text-muted" title="This blueprint is triggered by labelled GitHub issues.">
              GitHub issue trigger
            </span>
          )}
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
      </div>}
        {view === 'queue' && queue.length === 0 && <EmptyState label="Queue is empty" hint="Runs waiting to start will appear here." />}
        {view === 'queue' && queue.length > 0 && (
          <section className="border-b border-border-subtle p-2" data-testid="circuit-queue">
            <div className="flex items-baseline justify-between gap-2 mb-1.5">
              <h3 className="text-xs font-semibold text-text-primary">Queue</h3>
              <span className="text-2xs text-text-muted">Next to start first</span>
            </div>
            <ol className="flex flex-col gap-1">
              {queue.map((entry, index) => (
                <li
                  key={entry.run.id}
                  className="rounded-md border border-border-subtle bg-bg-card/40 p-1.5"
                  data-testid={`queue-run-${entry.run.id}`}
                >
                  <div className="flex items-start gap-1.5 min-w-0">
                    <span className="text-2xs font-mono text-text-secondary shrink-0">
                      {entry.queue_rank}
                    </span>
                    <span className="text-xs text-text-primary break-words min-w-0 flex-1">
                      {entry.circuit_name}{' '}
                      <span className="font-mono text-text-muted">#{entry.run.id}</span>
                    </span>
                  </div>
                  <div className="mt-1 flex flex-wrap items-center gap-1 pl-4">
                    <button
                      type="button"
                      onClick={() => runAction(() => moveCircuitRun(entry.run.id, 'up'))}
                      disabled={busy || index === 0}
                      aria-label={`Move run ${entry.run.id} up`}
                      className="px-1 text-xs text-text-muted hover:text-text-primary disabled:opacity-30"
                    >
                      ↑
                    </button>
                    <button
                      type="button"
                      onClick={() => runAction(() => moveCircuitRun(entry.run.id, 'down'))}
                      disabled={busy || index === queue.length - 1}
                      aria-label={`Move run ${entry.run.id} down`}
                      className="px-1 text-xs text-text-muted hover:text-text-primary disabled:opacity-30"
                    >
                      ↓
                    </button>
                    <button
                      type="button"
                      onClick={() => runAction(() => cancelCircuitRun(entry.run.id))}
                      disabled={busy}
                      aria-label={`Cancel run ${entry.run.id}`}
                      className="px-1.5 py-0.5 text-2xs rounded-md text-status-error hover:bg-status-error/10 disabled:opacity-40"
                    >
                      Cancel
                    </button>
                  </div>
                  <p className="mt-1 text-2xs font-mono text-text-muted break-all pl-4">
                    {entry.run.trigger_identity}
                  </p>
                  {/* Pending runs live in the queue (not the ledger); surface
                      the same admission detail a run card would so users
                      see *why* the run is parked (#1475). Reads from the
                      mesh-level budget the worker checks. */}
                  <p
                    className="mt-1 text-2xs text-text-muted break-words pl-4"
                    data-testid={`queue-pending-reason-${entry.run.id}`}
                  >
                    {pendingAdmissionDetail({
                      concurrencyLimit: 0,
                      runningSteps: 0,
                      meshRunCapacity,
                      meshActiveRuns,
                    })}
                  </p>
                </li>
              ))}
            </ol>
          </section>
        )}
        {view !== 'queue' && (rows.length === 0 ? (
          <div className="p-4">
            <EmptyState
              label="No circuits yet"
              hint="Open Manage to create a circuit in the flow editor."
            />
          </div>
        ) : (
          <ul className="flex flex-col gap-1 p-2">
            {visibleRows.length === 0 && <li><EmptyState label={view === 'history' ? 'No finished runs yet' : 'All caught up'} hint={view === 'history' ? 'Completed, failed and cancelled runs appear here.' : 'Active runs and failures appear here. Start a circuit from Manage.'} /></li>}
            {view === 'history' && <li className="text-2xs text-text-muted px-1">Recent history · up to 10 runs per circuit</li>}
            {visibleRows.map(({ circuit, runs, visibleRuns }) => {
              // One capacity snapshot per circuit, shared by its run cards
              // — `countRunningSteps` walks every run, so computing it
              // inside the run loop would be quadratic for nothing.
              // `meshRunCapacity` / `meshActiveRuns` are mesh-level and
              // pre-computed at component scope (#1475).
              const capacity = {
                concurrencyLimit: circuit.concurrency_limit,
                runningSteps: countRunningSteps(runs),
                meshRunCapacity,
                meshActiveRuns,
              };
              return (
              <li key={circuit.id} className="rounded-md border border-border-subtle p-2" data-testid="circuit-row">
                {circuit.is_preset ? (
                  <div className="flex items-baseline justify-between gap-2">
                    <span className="text-xs font-semibold text-text-primary">Agent review history</span>
                    <span className="text-2xs text-text-muted truncate" title={circuit.name}>{circuit.name}</span>
                  </div>
                ) : view !== 'manage' ? (
                  <h3 className="text-xs font-semibold text-text-primary break-words">{circuit.name}</h3>
                ) : (
                  <div className="flex flex-col items-stretch gap-1">
                    <label className="flex items-center gap-1.5 min-w-0">
                      <input
                        type="checkbox"
                        checked={circuit.enabled}
                        disabled={busy}
                        onChange={() =>
                          runAction(() => setCircuitEnabled(circuit.id, !circuit.enabled))
                        }
                        aria-label={`Enable ${circuit.name}`}
                        data-testid={`circuit-enabled-${circuit.id}`}
                      />
                      <span className="truncate text-text-primary" title={circuit.name}>
                        {circuit.name}
                      </span>
                    </label>
                    <div className="flex items-center gap-1 flex-wrap">
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
                        disabled={busy}
                        data-testid={`circuit-trigger-${circuit.id}`}
                        className="px-2 py-0.5 rounded-md bg-accent-cyan/15 text-accent-cyan hover:bg-accent-cyan/25 disabled:opacity-40"
                      >
                        Trigger Now
                      </button>
                      {confirmDeleteCircuitId === circuit.id ? (
                        <>
                          <button
                            type="button"
                            onClick={() => {
                              setConfirmDeleteCircuitId(null);
                              void runAction(() => deleteCircuit(circuit.id));
                            }}
                            disabled={busy}
                            aria-label={`Confirm delete ${circuit.name}`}
                            data-testid={`circuit-confirm-delete-${circuit.id}`}
                            className="px-1.5 py-0.5 rounded-md bg-status-error/15 text-status-error disabled:opacity-40"
                          >
                            Confirm
                          </button>
                          <button
                            type="button"
                            onClick={() => setConfirmDeleteCircuitId(null)}
                            disabled={busy}
                            aria-label={`Keep ${circuit.name}`}
                            className="px-1.5 py-0.5 rounded-md text-text-muted disabled:opacity-40"
                          >
                            Keep
                          </button>
                        </>
                      ) : (
                        <button
                          type="button"
                          onClick={() => setConfirmDeleteCircuitId(circuit.id)}
                          disabled={busy}
                          aria-label={`Delete ${circuit.name}`}
                          data-testid={`circuit-delete-${circuit.id}`}
                          className="px-1.5 py-0.5 rounded-md text-text-muted hover:text-status-error disabled:opacity-40"
                        >
                          Delete
                        </button>
                      )}
                    </div>
                  </div>
                )}

                {/* Run ledger — one expandable diagnostic card per run
                    (#1468), replacing the old truncated one-line chain. */}
                {view !== 'manage' && visibleRuns.length > 0 && (
                  <ul className="mt-1.5 flex flex-col gap-1" data-testid={`circuit-runs-${circuit.id}`}>
                    {visibleRuns.map((detail) => (
                      <CircuitRunCard
                        key={detail.run.id}
                        detail={detail}
                        capacity={capacity}
                        expanded={
                          runExpandOverrides[detail.run.id] ??
                          false
                        }
                        onToggleExpanded={() => toggleRunExpanded(detail.run.id)}
                        now={now}
                        busy={busy}
                        onPause={() => runAction(() => pauseCircuitRun(detail.run.id))}
                        onResume={() => runAction(() => resumeCircuitRun(detail.run.id))}
                        onCancel={() => runAction(() => cancelCircuitRun(detail.run.id))}
                        onApprove={(nodeId) =>
                          runAction(() => approveCircuitStep(detail.run.id, nodeId))
                        }
                      />
                    ))}
                  </ul>
                )}
              </li>
              );
            })}
          </ul>
        ))}
      </div>
    </div>
  );
}
