import { useMemo, useRef, useState } from 'react';
import { useAllAgentNodes, useAgentNodeStore } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { focusAgentNode } from '../../lib/focusAgentNode';
import { listAgentHistory, reopenAgentNode } from '../../lib/tauri/history';
import { formatError } from '../../lib/errorUtils';
import { formatListTimestamp } from '../../lib/time';
import { getNodeStatusConfig, STATUS_CONFIG } from '../../lib/status';
import { useAsyncEffect } from '../../hooks/useAsyncEffect';
import type { AgentNode } from '../../types/generated/AgentNode';
import { ArchivedNodesTab } from './ArchivedNodesTab';
import { ProbeTabBody } from './ProbeTabBody';
import { LoadingState, RefreshControl } from '../shared/Spinner';

// Issue #2461 — the card timestamp used to print the stored value verbatim
// (`2026-10-17T22:43:58.211596700Z`; the backend stores RFC 3339 at Rust
// nanosecond precision). Nine fractional digits inside a 240px panel is
// noise. The visible label is the same relative age the Archive list shows,
// and the exact instant moves to the tooltip so the sub-minute precision the
// raw string used to carry is still one hover away.
function absoluteTimestamp(iso: string): string | undefined {
  const then = new Date(iso);
  return Number.isNaN(then.getTime()) ? undefined : then.toLocaleString();
}

export function AgentHistoryTab() {
  const live = useAllAgentNodes();
  const meshes = useMeshStore(s => s.meshes);
  const adoptNode = useAgentNodeStore(s => s.adoptAgentNode);
  const spawn = useAgentNodeStore(s => s.spawnAgent);
  const [history, setHistory] = useState<AgentNode[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [reload, setReload] = useState(0);
  const [search, setSearch] = useState('');
  const [scope, setScope] = useState('');
  const [statusFilter, setStatusFilter] = useState('');
  const [discovered, setDiscovered] = useState(false);
  const [busy, setBusy] = useState<number | null>(null);
  const inFlight = useRef(false);
  function activate(id: number) {
    if (!focusAgentNode(id)) setError('This agent is no longer in the workspace. Refresh history before opening it.');
  }
  const identity = live.map(n => n.id).join(',');
  useAsyncEffect(signal => {
    setLoading(true); setError(null);
    void listAgentHistory().then(nodes => {
      if (signal.aborted) return;
      if (!Array.isArray(nodes)) throw new Error('Invalid agent history response');
      setHistory(nodes);
    }).catch(cause => { if (!signal.aborted) setError(formatError(cause)); })
      .finally(() => { if (!signal.aborted) setLoading(false); });
  }, [reload, identity]);
  const nodes = useMemo(() => {
    const merged = new Map(history.map(n => [n.id, n]));
    for (const node of live) merged.set(node.id, node);
    const query = search.trim().toLowerCase();
    return [...merged.values()].filter(n => {
      const mesh = meshes.find(m => m.id === n.mesh_id);
      return (!scope || n.mesh_id === Number(scope)) && (!statusFilter || n.status === statusFilter)
        && (!query || `${n.name} ${mesh?.name ?? ''} ${n.branch} ${n.cli_session_id ?? ''}`.toLowerCase().includes(query));
    }).sort((a, b) => b.created_at.localeCompare(a.created_at) || b.id - a.id);
  }, [history, live, scope, statusFilter, search, meshes]);
  const selectedMesh = meshes.find(m => String(m.id) === scope) ?? meshes[0];
  async function recover(node: AgentNode) {
    if (inFlight.current) return;
    inFlight.current = true; setBusy(node.id); setError(null);
    try {
      if (node.status === 'archived') { adoptNode(await reopenAgentNode(node.id)); }
      else { await spawn(node.id, node.provider); }
      activate(node.id);
    } catch (cause) { setError(formatError(cause)); }
    finally { inFlight.current = false; setBusy(null); }
  }
  const fieldClass = 'min-w-0 w-full rounded-md border border-border-default bg-bg-input px-2 py-1.5 text-xs text-text-primary';
  return <div className="flex h-full min-h-0 min-w-0 flex-col">
    <div className="shrink-0 space-y-2 border-b border-border-subtle p-3">
      <div className="flex gap-2">
        {/* Issue #2461 — the mode pair is one segmented group: `role="group"`
            carries the name to assistive technology (a generic div's
            `aria-label` is ignored — the same fix the Sidebar view switcher
            took in #2467), and the selected mode owns a background and text
            colour so the current view is visible rather than inferred from
            `aria-pressed`. Same active/inactive tokens as Sidebar. */}
        <div role="group" aria-label="Agent history view" className="flex gap-1">
          <button type="button" aria-pressed={!discovered} onClick={() => setDiscovered(false)} className={`min-h-[24px] rounded-md px-2 text-xs ${!discovered ? 'bg-bg-card text-text-primary font-medium' : 'text-text-secondary hover:bg-bg-card-hover'}`}>Lifecycle</button>
          <button type="button" aria-pressed={discovered} onClick={() => setDiscovered(true)} className={`min-h-[24px] rounded-md px-2 text-xs ${discovered ? 'bg-bg-card text-text-primary font-medium' : 'text-text-secondary hover:bg-bg-card-hover'}`}>Discovered sessions</button>
        </div>
        {/* Issue #2461 — Discovered sessions mounts `ArchivedNodesTab`, which
            renders its own `ProbeToolbar` and Refresh control (issue #813).
            A second one here stacked two Refresh buttons ~50px apart. */}
        {!discovered && <RefreshControl onRefresh={() => setReload(v => v + 1)} isRefreshing={loading} ariaLabel="Refresh agent history" />}
      </div>
      {/* Issue #2461 — the "All repositories" option is Lifecycle-only, so in
          Discovered sessions this select was a blank zero-option control above
          the "add a repository" empty state. With nothing to choose between
          there is no honest value to show: render the control only when it has
          an option. */}
      {(!discovered || meshes.length > 0) && <select aria-label="History repository" value={discovered ? String(selectedMesh?.id ?? '') : scope} className={fieldClass} onChange={e => setScope(e.target.value)}>
        {!discovered && <option value="">All repositories</option>}{meshes.map(m => <option key={m.id} value={m.id}>{m.name}</option>)}
      </select>}
      {!discovered && <>
        <input type="search" aria-label="Search agent history" placeholder="Agent, repository, branch or session" value={search} onChange={e => setSearch(e.target.value)} className={fieldClass} />
        <select aria-label="History status" value={statusFilter} onChange={e => setStatusFilter(e.target.value)} className={fieldClass}>
          <option value="">All lifecycle states</option>{Object.entries(STATUS_CONFIG).map(([value, config]) => <option key={value} value={value}>{config.label}</option>)}
        </select>
      </>}
    </div>
    {error && <div className="shrink-0 min-w-0 border-b border-border-subtle p-3">
      <div role="alert" className="max-h-24 overflow-y-auto overflow-x-hidden break-all text-xs text-status-error">Agent history needs attention: {error}</div>
      <button type="button" className="mt-2 min-h-[24px] rounded-md border border-border-default px-2 text-xs text-text-primary hover:bg-bg-card-hover" onClick={() => setReload(v => v + 1)}>Retry history</button>
    </div>}
    {discovered ? selectedMesh ? <ArchivedNodesTab key={`${selectedMesh.id}:${reload}`} meshId={selectedMesh.id} meshPath={selectedMesh.path} />
      : <p className="p-3 text-xs text-text-secondary">Add a repository to discover previous sessions.</p>
      : <ProbeTabBody>
        {loading && history.length === 0 && live.length === 0 ? <LoadingState label="Loading agent history…" /> : nodes.length === 0 ? <p className="text-xs text-text-secondary">{error ? 'Agent history unavailable.' : search || scope || statusFilter ? 'No matching agents. Clear search or filters to see more.' : 'No agent history yet.'}</p>
          : <div className="space-y-2">{nodes.map(node => {
            const config = getNodeStatusConfig(node);
            // Issue #2461 — `break-words`, not `break-all`: the label is prose
            // ("Created 3h ago") with word boundaries, and `break-all` split
            // the raw timestamp's trailing `Z` onto a line of its own.
            const observedAt = node.lifecycle?.timestamp ?? node.created_at;
            return <div key={node.id} className="min-w-0 rounded-md border border-border-default p-2">
              <p className="break-all text-sm text-text-primary">{node.name}</p>
              <p className="break-all text-xs text-text-secondary">{meshes.find(m => m.id === node.mesh_id)?.name ?? `Missing repository ${node.mesh_id}`} · {node.branch}</p>
              <p className={`text-xs ${config.color}`} title={config.title}>{config.label}</p>
              <p className="break-words text-2xs text-text-secondary" title={absoluteTimestamp(observedAt)}>{node.lifecycle ? 'Last observed' : 'Created'} {formatListTimestamp(observedAt)}</p>
              <div className="mt-2 flex flex-wrap gap-2">
                {node.status !== 'archived' && <button type="button" onClick={() => activate(node.id)} className="min-h-[24px] rounded-md border border-border-default px-2 text-xs text-text-primary hover:bg-bg-card-hover">Open terminal</button>}
                {['archived', 'suspended', 'error', 'lost'].includes(node.status) && <button type="button" disabled={busy !== null} onClick={() => void recover(node)}
                  title={node.status === 'archived' ? 'Restore this node to the workspace, preserving its session. Resume separately to start its process.' : 'Start the agent using its saved session when available'}
                  className="min-h-[24px] rounded-md border border-border-default px-2 text-xs text-text-primary hover:bg-bg-card-hover disabled:opacity-50">
                  {busy === node.id ? 'Opening…' : node.status === 'archived' ? 'Reopen' : node.status === 'suspended' ? 'Resume' : 'Retry'}
                </button>}
              </div>
            </div>;
          })}</div>}
      </ProbeTabBody>}
  </div>;
}
