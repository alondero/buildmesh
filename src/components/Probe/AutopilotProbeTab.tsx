import { useEffect, useRef, useState } from 'react';
import { useProbeContext } from '../../hooks/useProbeContext';
import { getMeshProperties, updateMeshCircuitRunCapacity } from '../../lib/tauri';
import type { MeshRow } from '../../types/generated/MeshRow';
import { useUIStore } from '../../stores/uiStore';
import { ProbeTabBody } from './ProbeTabBody';
import { LoadingState } from '../shared/Spinner';

export function AutopilotProbeTab() {
  const { activeMeshId } = useProbeContext();
  return activeMeshId === null
    ? <ProbeTabBody padding="p-3">Select a Mesh to inspect retained settings.</ProbeTabBody>
    : <RetainedSettings key={activeMeshId} meshId={activeMeshId} />;
}

function RetainedSettings({ meshId }: { meshId: number }) {
  const [row, setRow] = useState<MeshRow | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [pending, setPending] = useState(false);
  const mounted = useRef(true);
  useEffect(() => {
    mounted.current = true;
    let current = true;
    getMeshProperties(meshId).then(
      (value) => { if (current) setRow(value); },
      (cause: unknown) => { if (current) setError(String(cause)); },
    );
    return () => { current = false; mounted.current = false; };
  }, [meshId]);

  async function saveCapacity(value: number) {
    if (pending || !row) return;
    setPending(true);
    setError(null);
    try {
      await updateMeshCircuitRunCapacity(meshId, value);
      if (mounted.current) setRow({ ...row, circuit_run_capacity: value });
    } catch (cause) {
      if (mounted.current) setError(String(cause));
    } finally {
      if (mounted.current) setPending(false);
    }
  }

  return <ProbeTabBody padding="p-3" className="space-y-4">
    <p className="text-xs text-text-secondary">Legacy Autopilot is retired. Existing nodes, worktrees and history are retained. Legacy automation will not restart.</p>
    <button type="button" className="text-xs text-accent-cyan" onClick={() => useUIStore.getState().openProbeTab('circuits')}>Configure Circuits manually</button>
    {error && <p role="alert" className="text-xs text-status-error break-words">{error}</p>}
    {!row && !error && <LoadingState />}
    {row && <>
      <label className="block text-xs text-text-secondary">Max concurrent circuit runs
        <select value={row.circuit_run_capacity} disabled={pending}
          onChange={(event) => void saveCapacity(Number(event.target.value))}
          className="block mt-1 w-full rounded-sm border border-border-subtle bg-bg-card p-1">
          {[1, 2, 3, 4, 5, 6, 7, 8].map((value) => <option key={value} value={value}>{value}</option>)}
        </select>
      </label>
      <p className="text-2xs text-text-muted">One slot per admitted Circuit Run. Independent of the retained legacy concurrency setting.</p>
      <details className="text-xs break-words">
        <summary className="cursor-pointer">Retained legacy settings (read-only)</summary>
        <dl className="mt-2 space-y-1 text-text-secondary">
          <dt>Mode</dt><dd>{row.autopilot_mode === 'looping' ? 'Looping' : 'Issue-driven'}</dd>
          <dt>Trigger label</dt><dd>{row.autopilot_trigger_label ?? 'Default'}</dd>
          <dt>Legacy concurrency</dt><dd>{row.autopilot_concurrency_limit}</dd>
          <dt>Harness</dt><dd>{row.autopilot_provider ?? 'Mesh default'}</dd>
          <dt>On success</dt><dd>{row.autopilot_action_on_success ?? 'Draft PR'}</dd>
          <dt>Maximum iterations</dt><dd>{row.loop_max_iterations ?? 'Continuous'}</dd>
          <dt>Interval in seconds</dt><dd>{row.loop_interval_seconds}</dd>
          <dt>Consecutive failure limit</dt><dd>{row.loop_consecutive_failures}</dd>
          <dt>Initial prompt</dt><dd className="whitespace-pre-wrap">{row.loop_initial_prompt ?? 'Not set'}</dd>
          <dt>Suffix prompt</dt><dd className="whitespace-pre-wrap">{row.loop_suffix_prompt ?? 'Not set'}</dd>
        </dl>
      </details>
    </>}
  </ProbeTabBody>;
}
