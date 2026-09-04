import { useEffect, useState } from 'react';
import { createPortal } from 'react-dom';
import type { AgentNode } from '../../stores/agentNodeStore';
import { useAgentNodeStore } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { useUIStore } from '../../stores/uiStore';
import { listCircuits, triggerCircuitFromNode } from '../../lib/tauri';
import type { AutopilotCircuit } from '../../types/generated/AutopilotCircuit';
import type { CircuitGraph } from '../../types/generated/CircuitGraph';
import { Modal } from '../shared/Modal';
import { CircuitsIcon } from '../Probe/probeIcons';

export function AgentReviewButton({ node }: { node: AgentNode }) {
  const [open, setOpen] = useState(false);
  const [rounds, setRounds] = useState(3);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [circuits, setCircuits] = useState<AutopilotCircuit[]>([]);
  const [circuitId, setCircuitId] = useState<number | null>(null);
  const ownership = useAgentNodeStore(s => s.circuitOwnerships[node.id]);
  const eligible = node.provider !== 'terminal'
    && ['running', 'ready', 'awaiting_input', 'completed'].includes(node.status);

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
    useAgentNodeStore.getState().setActiveNode(node.id);
    useUIStore.getState().openProbeTab('circuits');
  }

  async function start() {
    setBusy(true);
    setError(null);
    try {
      await triggerCircuitFromNode(node.id, circuitId, rounds);
      setOpen(false);
      showRun();
    } catch (reason) {
      setError(String(reason));
    } finally {
      setBusy(false);
    }
  }

  return <>
    <button type="button"
      aria-label="Start agent workflow"
      title="Start a review loop or Circuit"
      disabled={!ownership && !eligible}
      onClick={() => setOpen(true)}
      className="p-1 rounded-md text-accent-violet hover:bg-accent-violet/15 disabled:opacity-40"
    ><CircuitsIcon className="h-4 w-4" /></button>
    {open && createPortal(
      <Modal onClose={() => { if (!busy) setOpen(false); }} ariaLabel="Start agent workflow" maxWidth="max-w-sm">
        <h2 className="text-sm font-semibold mb-2">Workflow for {node.name}</h2>
        {ownership && <button type="button" className="text-xs text-accent-violet mb-3"
          onClick={() => { setOpen(false); showRun(); }}>
          View circuit run #{ownership.run_id}
        </button>}
        <label className="text-xs block mb-4">Workflow
          <select value={circuitId ?? ''} onChange={e => setCircuitId(e.target.value ? Number(e.target.value) : null)}
            className="block w-full mt-1 bg-surface-raised border border-border-subtle rounded-md px-2 py-1">
            <option value="">Automated review loop</option>
            {circuits.map(circuit => <option key={circuit.id} value={circuit.id}>{circuit.name}</option>)}
          </select>
        </label>
        {circuitId === null ? <><p className="text-xs text-text-secondary mb-4">
          After this agent finishes its task, a separate reviewer checks its local changes.
          Findings return here for fixes and another review. The loop stops on approval or the round limit.
          The reviewer uses the same provider as this agent. You can pause or cancel in Circuits.
        </p>
        <label className="text-xs flex items-center justify-between gap-3 mb-4">
          Maximum review rounds
          <input type="number" min={1} max={10} value={rounds}
            onChange={e => setRounds(Number(e.target.value))}
            className="w-16 bg-surface-raised border border-border-subtle rounded-md px-2 py-1" />
        </label></> : <p className="text-xs text-text-secondary mb-4">
          Start this Circuit with {node.name} as its triggering agent. Its configured steps control when work starts.
          You can pause or cancel in Circuits.
        </p>}
        {error && <p role="alert" className="text-xs text-status-error break-words mb-3">{error}</p>}
        <div className="flex justify-end gap-2">
          <button type="button" disabled={busy} onClick={() => setOpen(false)} className="px-3 py-1.5 text-xs">Cancel</button>
          <button type="button" disabled={busy || !Number.isInteger(rounds) || rounds < 1 || rounds > 10}
            onClick={() => void start()}
            className="px-3 py-1.5 text-xs rounded-md bg-accent-violet/20 text-accent-violet disabled:opacity-40"
          >{busy ? 'Starting…' : circuitId === null ? 'Start review' : 'Start Circuit'}</button>
        </div>
      </Modal>, document.body)}
  </>;
}
