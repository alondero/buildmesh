import { PROBE_TAB_DEFINITIONS } from '../../lib/probeContext';
import { useProbeResource } from '../../lib/probeResource';
import type { AgentHistoryEntry } from '../../types/generated/AgentHistoryEntry';
import type { AgentNode } from '../../types/generated/AgentNode';
import { useMemo, useRef, useState } from 'react';
import { useAllAgentNodes, useAgentNodeStore } from '../../stores/agentNodeStore';
import { useMeshStore } from '../../stores/meshStore';
import { focusAgentNode } from '../../lib/focusAgentNode';
import { listAgentHistory, reopenAgentNode } from '../../lib/tauri/history';
import { formatError } from '../../lib/errorUtils';
import { formatListTimestamp, parseSqliteTimestamp } from '../../lib/time';
import { getNodeStatusConfig, STATUS_CONFIG } from '../../lib/status';
import { ArchivedNodesTab } from './ArchivedNodesTab';
import { ProbeTabBody } from './ProbeTabBody';
import { LoadingState, RefreshControl } from '../shared/Spinner';

// Issue #2461 — the card timestamp used to print the stored value verbatim
// (`2026-10-17T22:43:58.211596700Z`; the backend stores RFC 3339 at Rust
// nanosecond precision). Nine fractional digits inside a 240px panel is
// noise. The visible label is the same relative age the Archive list shows,
// and the exact instant moves to the tooltip so the sub-minute precision the
// raw string used to carry is still one hover away.
function absoluteTimestamp(at: Date | null): string | undefined {
  return at ? at.toLocaleString() : undefined;
}

/**
 * Parse one stored history timestamp.
 *
 * The column holds both RFC 3339 (status writes since migration v14) and
 * SQLite's zone-less `datetime('now')` text (the v14 backfill). Going through
 * `parseSqliteTimestamp` is what makes a legacy row read as UTC instead of
 * local time, so display and ordering agree on the instant. An unreadable
 * value is `null` rather than `Invalid Date`, which keeps it out of the
 * ordering's top slot.
 */
function parseHistoryTimestamp(value: string | null): Date | null {
  return value ? parseSqliteTimestamp(value) : null;
}

/** When the row last changed state, when that is known and readable. */
function changedAt(entry: AgentHistoryEntry): Date | null {
  return parseHistoryTimestamp(entry.status_changed_at);
}

/** When the worktree appeared. */
function createdAt(entry: AgentHistoryEntry): Date | null {
  return parseHistoryTimestamp(entry.created_at);
}

/** The durable instant Lifecycle rows are ordered by (issue #2801). */
export type HistoryOrder = 'changed' | 'created';

/**
 * The ordering key for one row, in epoch milliseconds.
 *
 * The default is `Last changed` with a creation fallback. `status_changed_at`
 * is the best durable record of when the work moved, but it is nullable (a
 * pre-v14 row) and a legacy write can leave it unparseable; both fall back to
 * `created_at` rather than to "now", because an unreadable timestamp must not
 * lift a stale row to the top of the list. `Created` is the explicit
 * alternative for spawn order. `null` sorts last.
 */
function orderTime(entry: AgentHistoryEntry, order: HistoryOrder): number | null {
  const changed = order === 'changed' ? changedAt(entry) : null;
  const at = changed ?? createdAt(entry);
  return at ? at.getTime() : null;
}

/**
 * Newest first, with a deterministic tie-break.
 *
 * Two rows can share a `status_changed_at` to the millisecond (a batch
 * archive, or a fallback that landed on the same creation instant). Ties fall
 * back to creation time and then to `id`, so a render and a reload agree on
 * the order instead of letting the browser's sort decide.
 */
function compareHistoryEntries(a: AgentHistoryEntry, b: AgentHistoryEntry, order: HistoryOrder): number {
  const aKey = orderTime(a, order);
  const bKey = orderTime(b, order);
  if (aKey !== bKey) {
    if (aKey === null) return 1;
    if (bKey === null) return -1;
    return bKey - aKey;
  }
  const aCreated = createdAt(a)?.getTime() ?? null;
  const bCreated = createdAt(b)?.getTime() ?? null;
  if (aCreated !== bCreated) {
    if (aCreated === null) return 1;
    if (bCreated === null) return -1;
    return bCreated - aCreated;
  }
  return b.id - a.id;
}

export function sortHistoryEntries(entries: AgentHistoryEntry[], order: HistoryOrder): AgentHistoryEntry[] {
  return [...entries].sort((a, b) => compareHistoryEntries(a, b, order));
}

/**
 * When this session last moved, and when its process started.
 *
 * `status_changed_at` is the closest thing the schema records to "when it
 * finished": for an archived node it is the archive instant. `created_at` is
 * only when the worktree appeared, which is why the card used to read as if
 * every session were created and finished at the same moment.
 *
 * Both columns are nullable (a pre-v14 row has no `status_changed_at`; a node
 * that never ran has no `session_started_at`), so each clause is omitted
 * rather than rendered blank.
 */
function sessionTiming(entry: AgentHistoryEntry): { label: string; exact?: string } {
  // The label follows the timestamp this row actually has: an unreadable
  // `status_changed_at` is the same absence ordering treats it as, so the
  // card says `Created …` instead of claiming a change instant it has no
  // readable value for.
  const changed = changedAt(entry);
  if (changed) {
    return {
      label: `${entry.status === 'archived' ? 'Archived' : 'Last changed'} ${formatListTimestamp(changed.toISOString())}`,
      exact: absoluteTimestamp(changed),
    };
  }
  const created = createdAt(entry);
  return {
    label: `Created ${created ? formatListTimestamp(created.toISOString()) : entry.created_at}`,
    exact: absoluteTimestamp(created),
  };
}

/** When the session's process started, when that is known. */
function sessionSpan(entry: AgentHistoryEntry): { label: string; exact?: string } | undefined {
  const started = entry.session_started_at;
  if (!started) return undefined;
  const at = new Date(started);
  if (Number.isNaN(at.getTime())) return undefined;
  return { label: `Started ${formatListTimestamp(at.toISOString())}`, exact: absoluteTimestamp(at) };
}

export async function loadHistory() {
  const entries = await listAgentHistory();
  if (!Array.isArray(entries)) throw new Error('Invalid agent history response');
  return entries;
}

export function AgentHistoryTab() {
  const live = useAllAgentNodes();
  const meshes = useMeshStore(s => s.meshes);
  const adoptNode = useAgentNodeStore(s => s.adoptAgentNode);
  const spawn = useAgentNodeStore(s => s.spawnAgent);
  const identity = live.map(n => n.id).join(',');
  const resource = useProbeResource(PROBE_TAB_DEFINITIONS.sessions.load, { meshId: null }, 'host', { refreshToken: identity });
  const { loading, error } = resource.state;
  const history = useMemo(() => resource.state.data ?? [], [resource.state.data]);
  const setError = resource.reportError;
  const [search, setSearch] = useState('');
  const [scope, setScope] = useState('');
  const [statusFilter, setStatusFilter] = useState('');
  const [order, setOrder] = useState<HistoryOrder>('changed');
  const [discovered, setDiscovered] = useState(false);
  const [busy, setBusy] = useState<number | null>(null);
  const inFlight = useRef(false);
  function activate(id: number) {
    if (!focusAgentNode(id)) setError('This agent is no longer in the workspace. Refresh history before opening it.');
  }
  const nodes = useMemo(() => {
    // Live nodes are merged over the history rows so a running session shows
    // its current status. A live node carries no `status_changed_at` on the
    // wire, so the history row's timing is kept when one exists — otherwise a
    // live node would lose the very timestamps this tab exists to show.
    const merged = new Map<number, AgentHistoryEntry>(history.map(e => [e.id, e]));
    for (const node of live) {
      const existing = merged.get(node.id);
      merged.set(node.id, existing ? { ...existing, ...node } : { ...node, status_changed_at: null, session_started_at: null });
    }
    const query = search.trim().toLowerCase();
    // Issue #2801 — ordering reads the loaded history row's durable
    // timestamps, not the merged live status. A terminal changes a live node's
    // status without touching those timestamps, and re-deriving the order from
    // it would move a row out from under the pointer or keyboard focus. A
    // reload (Refresh, or a node appearing/disappearing) re-sorts the list.
    return sortHistoryEntries([...merged.values()].filter(n => {
      const mesh = meshes.find(m => m.id === n.mesh_id);
      return (!scope || n.mesh_id === Number(scope)) && (!statusFilter || n.status === statusFilter)
        && (!query || `${n.name} ${mesh?.name ?? ''} ${n.branch} ${n.cli_session_id ?? ''}`.toLowerCase().includes(query));
    }), order);
  }, [history, live, scope, statusFilter, search, meshes, order]);
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
            `aria-label` is ignored), and the selected mode owns a background
            and text colour so the current view is visible rather than
            inferred from `aria-pressed`. */}
        <div role="group" aria-label="Agent history view" className="flex gap-1">
          <button type="button" aria-pressed={!discovered} onClick={() => setDiscovered(false)} className={`min-h-[24px] rounded-md px-2 text-xs ${!discovered ? 'bg-bg-card text-text-primary font-medium' : 'text-text-secondary hover:bg-bg-card-hover'}`}>Lifecycle</button>
          <button type="button" aria-pressed={discovered} onClick={() => setDiscovered(true)} className={`min-h-[24px] rounded-md px-2 text-xs ${discovered ? 'bg-bg-card text-text-primary font-medium' : 'text-text-secondary hover:bg-bg-card-hover'}`}>Discovered sessions</button>
        </div>
        {/* Issue #2461 — Discovered sessions mounts `ArchivedNodesTab`, which
            renders its own `ProbeToolbar` and Refresh control (issue #813).
            A second one here stacked two Refresh buttons ~50px apart. */}
        {!discovered && <RefreshControl onRefresh={() => void resource.reload()} isRefreshing={loading} ariaLabel="Refresh agent history" />}
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
        {/* Issue #2801 — the ordering control sits with the other Lifecycle
            filters and retains its value while they change. */}
        <select aria-label="History ordering" value={order} onChange={e => setOrder(e.target.value as HistoryOrder)} className={fieldClass}>
          <option value="changed">Last changed</option>
          <option value="created">Created</option>
        </select>
      </>}
    </div>
    {error && <div className="shrink-0 min-w-0 border-b border-border-subtle p-3">
      <div role="alert" className="max-h-24 overflow-y-auto overflow-x-hidden break-all text-xs text-status-error">Agent history needs attention: {error}</div>
      <button type="button" className="mt-2 min-h-[24px] rounded-md border border-border-default px-2 text-xs text-text-primary hover:bg-bg-card-hover" onClick={() => void resource.reload()}>Retry history</button>
    </div>}
    {discovered ? selectedMesh ? <ArchivedNodesTab key={selectedMesh.id} meshId={selectedMesh.id} meshPath={selectedMesh.path} />
      : <p className="p-3 text-xs text-text-secondary">Add a repository to discover previous sessions.</p>
      : <ProbeTabBody>
        {loading && history.length === 0 && live.length === 0 ? <LoadingState label="Loading agent history…" /> : nodes.length === 0 ? <p className="text-xs text-text-secondary">{error ? 'Agent history unavailable.' : search || scope || statusFilter ? 'No matching agents. Clear search or filters to see more.' : 'No agent history yet.'}</p>
          : <div className="space-y-2">{nodes.map(node => {
            const config = getNodeStatusConfig(node);
            // Issue #2461 — `break-words`, not `break-all`: the label is prose
            // ("Created 3h ago") with word boundaries, and `break-all` split
            // the raw timestamp's trailing `Z` onto a line of its own.
            const timing = sessionTiming(node);
            const span = sessionSpan(node);
            return <div key={node.id} className="min-w-0 rounded-md border border-border-default p-2">
              <p className="break-all text-sm text-text-primary">{node.name}</p>
              <p className="break-all text-xs text-text-secondary">{meshes.find(m => m.id === node.mesh_id)?.name ?? `Missing repository ${node.mesh_id}`} · {node.branch}</p>
              <p className={`text-xs ${config.color}`} title={config.title}>{config.label}</p>
              <p className="break-words text-2xs text-text-secondary" title={timing.exact}>{timing.label}</p>
              {span && <p className="break-words text-2xs text-text-secondary" title={span.exact}>{span.label}</p>}
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
