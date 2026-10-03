import { useRef, useState } from 'react';
import { useAllAgentNodes, useAgentNodeStore } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { focusAgentNode } from '../../lib/focusAgentNode';
import { useToastStore } from '../../stores/toastStore';
import { getNodeStatusConfig, needsAgentAttention } from '../../lib/status';
import { formatError } from '../../lib/errorUtils';

export function AttentionList() {
  const nodes = useAllAgentNodes();
  const meshes = useMeshStore(s => s.meshes);
  function activate(id: number) {
    if (!focusAgentNode(id)) useToastStore.getState().addToast('Agent unavailable', 'This agent is no longer in the workspace. Refresh Agent History to find archived work.', 'error');
  }
  const spawn = useAgentNodeStore(s => s.spawnAgent);
  const inFlight = useRef(new Set<number>());
  const [busy, setBusy] = useState(new Set<number>());
  const attention = nodes.filter(n => needsAgentAttention(n.status))
    .sort((a, b) => Number(b.status === 'error') - Number(a.status === 'error') || a.id - b.id);
  async function resume(id: number, provider: string) {
    if (inFlight.current.has(id)) return;
    inFlight.current.add(id);
    setBusy(new Set(inFlight.current));
    try { await spawn(id, provider); activate(id); }
    catch (error) { useToastStore.getState().addToast('Recovery failed', formatError(error), 'error'); }
    finally { inFlight.current.delete(id); setBusy(new Set(inFlight.current)); }
  }
  return (
    <div className="space-y-2" aria-label="Agents needing attention">
      {attention.length === 0 && <p role="status" className="px-2 py-4 text-xs text-text-secondary">No agents need attention.</p>}
      {attention.map(node => {
        const status = getNodeStatusConfig(node);
        const mesh = meshes.find(m => m.id === node.mesh_id);
        return <div key={node.id} className="min-w-0 rounded-md border border-border-default bg-bg-card p-2">
          <button type="button" onClick={() => activate(node.id)} title={node.name}
            className="w-full min-w-0 text-left text-sm text-text-primary hover:text-accent-cyan">
            <span className="block break-words [overflow-wrap:anywhere]">{node.name}</span>
            <span className="block break-all text-xs text-text-secondary">{mesh?.name ?? `Repository ${node.mesh_id}`}</span>
          </button>
          <p className={`mt-1 text-xs ${status.color}`} title={status.title}>{status.label}</p>
          <div className="mt-2 flex flex-wrap gap-2">
            <button type="button" onClick={() => activate(node.id)} className="min-h-[24px] rounded-md border border-border-default px-2 text-xs text-text-primary hover:bg-bg-card-hover">Open terminal</button>
            {(node.status === 'error' || node.status === 'lost' || node.status === 'suspended') && <button type="button" disabled={busy.has(node.id)}
              aria-label={`${node.status === 'suspended' ? 'Resume' : 'Retry'} ${node.name}`}
              onClick={() => void resume(node.id, node.provider)}
              className="min-h-[24px] rounded-md border border-border-default px-2 text-xs text-text-primary hover:bg-bg-card-hover disabled:opacity-50">
              {busy.has(node.id) ? 'Starting…' : node.status === 'suspended' ? 'Resume' : 'Retry'}
            </button>}
          </div>
        </div>;
      })}
    </div>
  );
}
