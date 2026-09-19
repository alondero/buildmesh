import { useEffect, useMemo, useState } from 'react';
import { createPortal } from 'react-dom';
import type { AgentNode } from '../../stores/agentNodeStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useNodeActivityStore } from '../../stores/nodeActivityStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { cancelCircuitRun, isAgentRunning, listCircuits, triggerCircuitFromNode } from '../../lib/tauri';
import type { SpawnOption } from '../../lib/groups';
import { groupByHarness } from '../../lib/groups';
import type { AutopilotCircuit } from '../../types/generated/AutopilotCircuit';
import type { CircuitGraph } from '../../types/generated/CircuitGraph';
import { Modal } from '../shared/Modal';
import { CircuitsIcon } from '../Probe/probeIcons';

export function AgentReviewButton({ node, providerList }: { node: AgentNode; providerList: SpawnOption[] }) {
  const [open, setOpen] = useState(false);
  const [rounds, setRounds] = useState(3);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [circuits, setCircuits] = useState<AutopilotCircuit[]>([]);
  const [circuitId, setCircuitId] = useState<number | null>(null);
  const [reviewerProvider, setReviewerProvider] = useState('');
  // Same bucketing the Spawn Menu renders (ADR-0016): harness headers with
  // their Proxied children nested, which keeps composite `harness:provider`
  // rows unambiguous. Terminal is not an agent, so it is filtered out before
  // bucketing — the backend enforces the same invariant.
  const reviewerGroups = useMemo(
    () => groupByHarness(providerList, { filter: provider => provider.harness_id !== 'terminal' }),
    [providerList],
  );
  const ownership = useAgentNodeStore(s => s.circuitOwnerships[node.id]);
  const activeOwnership = ownership && ['pending', 'running', 'paused'].includes(ownership.state)
    ? ownership
    : null;
  // The backend's gate (`commands::circuit::trigger_circuit_from_node`) trusts
  // `PROCESS_REGISTRY.is_alive(&node_id)` over the DB status, so a node whose
  // process is up but whose status is still `pending`/`spawning` (transient
  // row, or one stuck on `Starting…` because of a fire-and-forget swallow
  // upstream) is still eligible. We poll `isAgentRunning` while the node is in
  // a transient state so the button flips enabled the moment the process is
  // actually registered. Outside transient states the DB status is the
  // cheaper source of truth.
  const statusEligible = node.status === 'running'
    || node.status === 'ready'
    || node.status === 'awaiting_input'
    || node.status === 'completed';
  const pollingLiveness = node.status === 'pending' || node.status === 'spawning';
  const processAlive = useAgentProcessAlive(node.id, pollingLiveness);
  const eligible = node.provider !== 'terminal' && (statusEligible || processAlive);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    listCircuits(node.mesh_id).then(rows => {
      if (cancelled) return;
      setCircuits(rows.filter(row => {
        try {
          const graph = JSON.parse(row.graph_json) as CircuitGraph;
          const roots = graph.nodes.filter(n => !graph.edges.some(e => e.to === n.id));
          return roots.length > 0 && roots.every(n => n.type.type === 'manual');
        } catch { return false; }
      }));
    }).catch(reason => { if (!cancelled) setError(String(reason)); });
    return () => { cancelled = true; };
  }, [open, node.mesh_id]);

  function showRun() {
    useMeshStore.getState().selectMesh(node.mesh_id);
    useNodeActivityStore.getState().activateNode(node.id);
    useUIStore.getState().openProbeTab('circuits');
  }

  async function start() {
    setBusy(true);
    setError(null);
    try {
      await triggerCircuitFromNode(node.id, circuitId, rounds, circuitId === null ? reviewerProvider || null : null);
      setOpen(false);
      showRun();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  async function cancelActive() {
    if (!activeOwnership) return;
    setBusy(true);
    setError(null);
    try {
      await cancelCircuitRun(activeOwnership.run_id);
      setOpen(false);
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  return <>
    <button type="button"
      aria-label="Start review or circuit"
      title="Start review or circuit"
      disabled={!activeOwnership && !eligible}
      onClick={() => { setReviewerProvider(''); setOpen(true); }}
      className="p-1 rounded-md text-accent-violet hover:bg-accent-violet/15 disabled:opacity-40"
    ><CircuitsIcon className="h-4 w-4" /></button>
    {open && createPortal(
      <Modal onClose={() => { if (!busy) setOpen(false); }} ariaLabel="Start review or circuit" maxWidth="max-w-sm">
        <h2 className="text-sm font-semibold mb-2">Review or circuit for {node.name}</h2>
        {activeOwnership ? <>
          <p className="text-xs text-text-secondary mb-3">
            This agent is already controlled by Circuit #{activeOwnership.run_id} ({activeOwnership.state}).
          </p>
          <button type="button" className="text-xs text-accent-violet mb-3"
            onClick={() => {
              setOpen(false);
              // The Probe follows mesh selection; scope it to this node's mesh
              // so the circuit run is in the loaded snapshot.
              useMeshStore.getState().selectMesh(node.mesh_id);
              useUIStore.getState().focusCircuitRun(activeOwnership.run_id);
            }}>
            View circuit run #{activeOwnership.run_id}
          </button>
          <button type="button" disabled={busy} onClick={() => void cancelActive()}
            className="block text-xs text-status-error mb-4 disabled:opacity-40">
            Cancel active workflow
          </button>
        </> : <>
        <label className="text-xs block mb-4">Workflow
          <select value={circuitId ?? ''} onChange={e => setCircuitId(e.target.value ? Number(e.target.value) : null)}
            className="block w-full mt-1 bg-bg-overlay border border-border-subtle rounded-md px-2 py-1 text-text-primary">
            <option value="">Automated review loop</option>
            {circuits.map(circuit => <option key={circuit.id} value={circuit.id}>{circuit.name}</option>)}
          </select>
        </label>
        {circuitId === null ? <>
        <label className="text-xs block mb-4">Reviewer provider
          <select value={reviewerProvider}
            onChange={e => setReviewerProvider(e.target.value)}
            className="block w-full mt-1 bg-bg-overlay border border-border-subtle rounded-md px-2 py-1 text-text-primary">
            <option value="">Default (app Reviewer provider or this agent)</option>
            {reviewerGroups.map(([groupKey, rows]) => (
              <optgroup key={groupKey} label={rows.find(row => !row.is_proxied)?.label ?? groupKey}>
                {rows.map(provider => (
                  <option key={provider.id} value={provider.id}>{provider.label}</option>
                ))}
              </optgroup>
            ))}
          </select>
        </label>
        <p className="text-xs text-text-secondary mb-4">
          After this agent finishes its task, a separate reviewer checks its local changes.
          Findings return here for fixes and another review. The loop stops on approval or the round limit.
          The reviewer uses the app-wide Reviewer provider when configured, otherwise this
          agent's provider — pick a provider above to override it for this review. Model and
          effort come from the picked provider's own harness configuration.
          You can pause or cancel in Circuits.
        </p>
        <label className="text-xs flex items-center justify-between gap-3 mb-4">
          Maximum review rounds
          <input type="number" min={1} max={10} value={rounds}
            onChange={e => setRounds(Number(e.target.value))}
            className="w-16 bg-bg-overlay border border-border-subtle rounded-md px-2 py-1 text-text-primary" />
        </label></> : <p className="text-xs text-text-secondary mb-4">
          Start this Circuit with {node.name} as its triggering agent. Its configured steps control when work starts.
          You can pause or cancel in Circuits.
        </p>}
        </>}
        {error && <p role="alert" className="text-xs text-status-error break-words mb-3">{error}</p>}
        <div className="flex justify-end gap-2">
          <button type="button" disabled={busy} onClick={() => setOpen(false)} className="px-3 py-1.5 text-xs">Cancel</button>
          {!activeOwnership && (
          <button type="button" disabled={busy || !Number.isInteger(rounds) || rounds < 1 || rounds > 10}
            onClick={() => void start()}
            className="px-3 py-1.5 text-xs rounded-md bg-accent-violet/20 text-accent-violet disabled:opacity-40"
          >{busy ? 'Starting…' : circuitId === null ? 'Start review' : 'Start Circuit'}</button>
          )}
        </div>
      </Modal>, document.body)}
  </>;
}

/**
 * Reactively reports whether `nodeId` has a live agent process, polled while
 * `active` is true and skipped entirely otherwise. Used by
 * [`AgentReviewButton`] to mirror the backend's actual safety gate
 * (`PROCESS_REGISTRY.is_alive`) instead of the DB status alone — a row
 * stranded on `pending`/`spawning` by an upstream fire-and-forget swallow
 * still lights up as eligible the instant its PTY reader is registered.
 */
export function useAgentProcessAlive(nodeId: number, active: boolean): boolean {
  const [alive, setAlive] = useState<boolean>(false);

  useEffect(() => {
    if (!active) {
      // Reset to the safe default when polling stops so a transient-to-stable
      // transition cannot leave the previous poll's `true` lingering past the
      // point where the consumer stops trusting it.
      setAlive(false);
      return;
    }
    let cancelled = false;
    const check = async () => {
      try {
        const result = await isAgentRunning(nodeId);
        if (!cancelled) setAlive(result);
      } catch {
        // The backend may transiently reject during startup races; treat as
        // "unknown, not alive" and let the next poll retry.
        if (!cancelled) setAlive(false);
      }
    };
    void check();
    const id = window.setInterval(check, 1000);
    return () => { cancelled = true; window.clearInterval(id); };
  }, [nodeId, active]);

  return alive;
}
