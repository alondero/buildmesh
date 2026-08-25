/**
 * RunHistoryDrawer — the editor's run-history drawer (issue #1209).
 *
 * Lists the circuit's active and past runs (newest first). Selecting
 * one highlights its exact traversed path through the DAG (the parent
 * computes the highlight sets) and shows per-step ledger detail:
 * status, outcome, duration, and any error message.
 */

import type { CircuitRunDetail } from '../../lib/tauri';
import { statusTextClass, stepDurationMs } from './circuitGraphModel';

interface RunHistoryDrawerProps {
  runs: CircuitRunDetail[];
  selectedRunId: number | null;
  onSelectRun: (runId: number) => void;
}

export function RunHistoryDrawer({ runs, selectedRunId, onSelectRun }: RunHistoryDrawerProps) {
  const selected = runs.find((r) => r.run.id === selectedRunId) ?? null;
  return (
    <aside
      data-testid="run-history-drawer"
      className="w-72 shrink-0 border-l border-border-subtle bg-bg-surface flex flex-col overflow-hidden"
    >
      <div className="px-3 py-2 border-b border-border-subtle text-xs font-semibold text-text-primary">
        Run History
      </div>
      <ul className="max-h-48 overflow-y-auto border-b border-border-subtle">
        {runs.length === 0 && (
          <li className="px-3 py-2 text-2xs text-text-muted">No runs yet.</li>
        )}
        {runs.map(({ run }) => (
          <li key={run.id}>
            <button
              type="button"
              data-testid={`history-run-${run.id}`}
              onClick={() => onSelectRun(run.id)}
              className={`w-full text-left px-3 py-1 text-xs hover:bg-bg-card-hover ${
                run.id === selectedRunId ? 'bg-accent-cyan/10' : ''
              }`}
            >
              <span className="text-text-muted mr-1">#{run.id}</span>
              <span
                className={`${statusTextClass(run.state)} ${run.state === 'running' ? 'animate-pulse' : ''}`}
              >
                {run.state}
              </span>
              <span className="ml-1 text-2xs text-text-muted">{run.created_at}</span>
            </button>
          </li>
        ))}
      </ul>
      {selected && (
        <div className="flex-1 overflow-y-auto p-2" data-testid={`run-steps-${selected.run.id}`}>
          {selected.steps.length === 0 && (
            <p className="text-2xs text-text-muted px-1">No steps recorded.</p>
          )}
          {selected.steps.map((s) => {
            const duration = stepDurationMs(s);
            return (
              <div
                key={s.node_id}
                data-testid={`run-step-${selected.run.id}-${s.node_id}`}
                className="mb-1 rounded-sm border border-border-subtle bg-bg-card p-1.5 text-2xs"
              >
                <div className="flex items-center justify-between gap-1">
                  <span className="font-mono text-text-primary">{s.node_id}</span>
                  <span
                    className={`${statusTextClass(s.status)} ${s.status === 'running' ? 'animate-pulse' : ''}`}
                  >
                    {s.status}
                    {s.outcome ? ` · ${s.outcome}` : ''}
                  </span>
                </div>
                {duration !== null && (
                  <div className="text-text-muted mt-0.5">duration {(duration / 1000).toFixed(1)}s</div>
                )}
                {s.agent_node_id !== null && (
                  <div className="text-text-muted mt-0.5">agent node #{s.agent_node_id}</div>
                )}
                {s.error_message && (
                  // Per-step log surface: the classifier/PTY error text.
                  <pre className="mt-0.5 whitespace-pre-wrap text-status-error font-mono">
                    {s.error_message}
                  </pre>
                )}
              </div>
            );
          })}
        </div>
      )}
    </aside>
  );
}
