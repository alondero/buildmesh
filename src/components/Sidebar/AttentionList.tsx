import { useRef, useState } from 'react';
import { useAllAgentNodes, useAgentNodeStore } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { focusAgentNode } from '../../lib/focusAgentNode';
import { useToastStore } from '../../stores/toastStore';
import { getNodeStatusConfig, needsAgentAttention, nodeInputContext } from '../../lib/status';
import { formatError } from '../../lib/errorUtils';
import { canResumeSuspendedNode } from '../../lib/suspended';
import type { SessionStatus } from '../../types/generated/SessionStatus';

const ATTENTION_GROUPS: { id: string; label: string; statuses: SessionStatus[] }[] = [
  { id: 'failed', label: 'Failed sessions', statuses: ['error', 'lost'] },
  { id: 'input', label: 'Needs input', statuses: ['awaiting_input'] },
  { id: 'paused', label: 'Paused sessions', statuses: ['suspended'] },
];

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
    .sort((a, b) => a.id - b.id);
  async function resume(id: number, provider: string) {
    if (inFlight.current.has(id)) return;
    inFlight.current.add(id);
    setBusy(new Set(inFlight.current));
    try { await spawn(id, provider); activate(id); }
    catch (error) { useToastStore.getState().addToast('Recovery failed', formatError(error), 'error'); }
    finally { inFlight.current.delete(id); setBusy(new Set(inFlight.current)); }
  }
  return (
    <div className="space-y-4" aria-label="Agents needing attention">
      {attention.length === 0 && <p role="status" className="px-2 py-4 text-xs text-text-secondary">No agents need attention.</p>}
      {ATTENTION_GROUPS.map(group => {
        const members = attention.filter(node => group.statuses.includes(node.status));
        if (members.length === 0) return null;
        return <section key={group.id} aria-labelledby={`attention-${group.id}`}>
          <h3 id={`attention-${group.id}`} className="mb-2 flex items-center justify-between gap-2 px-1 text-xs font-semibold text-text-secondary">
            {group.label}{' '}<span className="tabular-nums">{members.length}</span>
          </h3>
          <ul className="space-y-2">
            {members.map(node => {
              const status = getNodeStatusConfig(node);
              const mesh = meshes.find(m => m.id === node.mesh_id);
              const context = nodeInputContext(node);
              const recoverable = node.status === 'error' || node.status === 'lost' || canResumeSuspendedNode(node);
              return <li key={node.id} className="min-w-0 rounded-md border border-border-default bg-bg-card p-2.5">
                <p className={`mb-1 text-xs font-medium ${status.color}`} title={status.title}>{status.label}</p>
                <h4 className="break-words text-sm font-semibold text-text-primary [overflow-wrap:anywhere]">{node.name}</h4>
                <p className="mt-0.5 break-words text-xs text-text-secondary [overflow-wrap:anywhere]">{mesh?.name ?? `Repository ${node.mesh_id}`}</p>
                {context?.trim() && <p className="mt-2 whitespace-pre-wrap break-words text-xs text-text-primary [overflow-wrap:anywhere]">{context}</p>}
                <div className="mt-2 flex flex-wrap gap-2">
                  <button type="button" onClick={() => activate(node.id)} aria-label={`Open terminal for ${node.name}`}
                    className="min-h-[28px] rounded-md border border-border-default px-2 text-xs font-medium text-accent-cyan hover:bg-bg-card-hover">Open terminal</button>
                  {recoverable && <button type="button" disabled={busy.has(node.id)}
                    aria-label={`${node.status === 'suspended' ? 'Resume' : 'Retry'} ${node.name}`}
                    onClick={() => void resume(node.id, node.provider)}
                    className="min-h-[28px] rounded-md px-2 text-xs text-text-secondary hover:bg-bg-card-hover disabled:opacity-50">
                    {busy.has(node.id) ? 'Starting…' : node.status === 'suspended' ? 'Resume' : 'Retry'}
                  </button>}
                </div>
              </li>;
            })}
          </ul>
        </section>;
      })}
    </div>
  );
}
