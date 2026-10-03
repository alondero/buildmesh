import { useRef, useState } from 'react';
import { useUIStore, type GridSortBy, type GridSortDirection } from '../../stores/uiStore';
import { useAllAgentNodes } from '../../stores/agentNodeStore';
import { STATUS_CONFIG } from '../../lib/status';
import { matchesGridControls } from '../../lib/viewModes';
import type { SessionStatus } from '../../types/generated/SessionStatus';
import { useClickOutside } from '../../hooks/useClickOutside';

export function GridFilterPopover() {
  const query = useUIStore(s => s.gridSearchQuery);
  const provider = useUIStore(s => s.gridProviderFilter);
  const status = useUIStore(s => s.gridStatusFilter);
  const sort = useUIStore(s => s.gridSortBy);
  const direction = useUIStore(s => s.gridSortDirection);
  const nodes = useAllAgentNodes();
  const [open, setOpen] = useState(false);
  const trigger = useRef<HTMLButtonElement>(null);
  useClickOutside(open ? 'grid-filters' : null, () => setOpen(false));
  const controls = { gridSearchQuery: query, gridProviderFilter: provider, gridStatusFilter: status };
  const count = nodes.filter(n => matchesGridControls(n, controls)).length;
  const providers = [...new Set([...nodes.map(n => n.provider), ...(provider ? [provider] : [])])].sort();
  const activeCount = Number(Boolean(query.trim())) + Number(provider !== null) + Number(status !== null);
  const changed = activeCount > 0 || sort !== 'custom' || direction !== 'asc';
  const actions = useUIStore.getState();
  const fieldClass = 'w-full min-w-0 rounded-md border border-border-default bg-bg-input px-2 py-1.5 text-text-primary';
  return (
    <div className="relative flex shrink-0 items-center gap-1" data-dropdown-for="grid-filters">
      <button ref={trigger} type="button" aria-label={`Filters and sort: ${count} of ${nodes.length} nodes, ${activeCount} active filters`}
        aria-expanded={open} aria-controls="grid-filter-panel" title="Filters and sort"
        onClick={() => setOpen(v => !v)}
        className="flex min-h-[24px] items-center gap-1 rounded-md px-1.5 text-xs text-text-secondary hover:bg-bg-card-hover">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true"><path d="M3 5h18M6 12h12M10 19h4" /></svg>
        <span aria-live="polite">{count}/{nodes.length}</span>
        {activeCount > 0 && <span className="text-accent-cyan">({activeCount})</span>}
      </button>
      {changed && <button type="button" aria-label="Clear all filters and sorting" title="Clear all"
        onClick={actions.resetGridControls} className="min-h-[24px] min-w-[24px] rounded-md text-text-secondary hover:bg-bg-card-hover">×</button>}
      {open && (
        <div id="grid-filter-panel" role="region" aria-label="Filters and sort"
          className="absolute left-0 top-full z-50 mt-2 w-72 max-w-[calc(100vw-16px)] space-y-3 rounded-md border border-border-default bg-bg-overlay p-3 text-xs shadow-lg"
          onKeyDown={e => { if (e.key === 'Escape') { e.stopPropagation(); setOpen(false); trigger.current?.focus(); } }}>
          <p className="text-text-secondary" role="status">{count} of {nodes.length} nodes</p>
          <label className="block text-text-secondary">Provider
            <select aria-label="Filter provider" value={provider ?? ''} className={fieldClass}
              onChange={e => actions.setGridProviderFilter(e.target.value || null)}>
              <option value="">All providers</option>{providers.map(p => <option key={p} value={p}>{p}</option>)}
            </select>
          </label>
          <label className="block text-text-secondary">Status
            <select aria-label="Filter status" value={status ?? ''} className={fieldClass}
              onChange={e => actions.setGridStatusFilter(e.target.value ? e.target.value as SessionStatus : null)}>
              <option value="">All statuses</option>{Object.entries(STATUS_CONFIG).map(([key, value]) => <option key={key} value={key}>{value.label}</option>)}
            </select>
          </label>
          <div className="grid grid-cols-2 gap-2">
            <label className="text-text-secondary">Sort
              <select aria-label="Sort nodes" value={sort} className={fieldClass} onChange={e => actions.setGridSortBy(e.target.value as GridSortBy)}>
                <option value="custom">Manual order</option><option value="name">Name</option><option value="status">Status</option><option value="created">Created</option>
              </select>
            </label>
            <label className="text-text-secondary">Direction
              <select aria-label="Sort direction" value={direction} disabled={sort === 'custom'} className={fieldClass} onChange={e => actions.setGridSortDirection(e.target.value as GridSortDirection)}>
                <option value="asc">Ascending</option><option value="desc">Descending</option>
              </select>
            </label>
          </div>
          <div className="flex flex-wrap gap-1" aria-label="Active filters">
            {query.trim() && <button title="Remove text filter" className="max-w-full break-all rounded-md bg-bg-card px-2 py-1 text-text-primary" onClick={() => actions.setGridSearchQuery('')}>Search: {query} ×</button>}
            {provider && <button className="max-w-full break-all rounded-md bg-bg-card px-2 py-1 text-text-primary" onClick={() => actions.setGridProviderFilter(null)}>Provider: {provider} ×</button>}
            {status && <button className="rounded-md bg-bg-card px-2 py-1 text-text-primary" onClick={() => actions.setGridStatusFilter(null)}>Status: {STATUS_CONFIG[status].label} ×</button>}
          </div>
          <button type="button" onClick={actions.resetGridControls} className="rounded-md border border-border-default px-3 py-1.5 text-text-primary hover:bg-bg-card-hover">Clear all</button>
        </div>
      )}
    </div>
  );
}
