import { useRef, useState } from 'react';
import { useUIStore, type GridSortBy, type GridSortDirection } from '../../stores/uiStore';
import { useAllAgentNodes } from '../../stores/agentNodeStore';
import { STATUS_CONFIG } from '../../lib/status';
import { useScope } from '../../hooks/useScope';
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
  const scope = useScope();
  const count = scope.visibleNodeCount;
  const scopedCount = scope.scopedNodeCount;
  const providers = [...new Set([...nodes.map(n => n.provider), ...(provider ? [provider] : [])])].sort();
  const activeCount = Number(Boolean(query.trim())) + Number(provider !== null) + Number(status !== null);
  const changed = activeCount > 0 || sort !== 'custom' || direction !== 'asc';
  const actions = useUIStore.getState();
  const fieldClass = 'w-full min-w-0 rounded-md border border-border-default bg-bg-input px-2 py-1.5 text-text-primary';
  // Issue #2774 — one shape for the three removable filter chips, so the
  // surface hover token, the pressed state and the 24 CSS-pixel minimum
  // target stay in step across them.
  const chipClass = 'flex min-h-[24px] min-w-0 max-w-full items-center rounded-md bg-bg-card px-2 py-1 text-text-primary transition-colors hover:bg-bg-card-hover active:bg-bg-card-hover';
  return (
    <div className="relative flex shrink-0 items-center gap-1" data-dropdown-for="grid-filters"
      onKeyDown={e => { if (open && e.key === 'Escape') { e.stopPropagation(); setOpen(false); trigger.current?.focus(); } }}>
      <button ref={trigger} type="button" aria-label={`Filters and sort: ${count} of ${scopedCount} nodes, ${activeCount} active filters`}
        aria-expanded={open} aria-controls="grid-filter-panel" title="Filters and sort"
        onClick={() => setOpen(v => !v)}
        className="flex min-h-[24px] items-center gap-1 rounded-md px-1.5 text-xs text-text-secondary hover:bg-bg-card-hover">
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" aria-hidden="true"><path d="M3 5h18M6 12h12M10 19h4" /></svg>
        <span aria-live="polite">{count}/{scopedCount}</span>
        {activeCount > 0 && <span className="text-accent-cyan">({activeCount})</span>}
      </button>
      {changed && <button type="button" aria-label="Clear all filters and sorting" title="Clear all"
        onClick={actions.resetGridControls} className="min-h-[24px] min-w-[24px] rounded-md text-text-secondary hover:bg-bg-card-hover">×</button>}
      {open && (
        <div id="grid-filter-panel" role="region" aria-label="Filters and sort"
          className="absolute left-0 top-full z-50 mt-2 w-72 max-w-[calc(100vw-16px)] space-y-3 rounded-md border border-border-default bg-bg-overlay p-3 text-xs shadow-lg">
          <p className="text-text-secondary" role="status">{count} of {scopedCount} nodes</p>
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
            {/* Issue #2774 — a removable filter chip is a compact desktop
                control, so it carries the same surface hover token as the
                rest of the panel: the chips sat on `bg-bg-card` and the
                hover rule did not exist, so in light mode Search stayed
                `rgb(245,245,247)` with `rgb(15,23,42)` text on hover — no
                computed change at all. `active:` gives the pressed state
                the same wash, and `min-h-[24px]` restores the 24 CSS-pixel
                minimum target. The visible label still names the filter and
                its value; the accessible name says what pressing it does,
                because "Provider: anthropic ×" alone does not tell a
                screen reader that the chip removes the filter. */}
            {query.trim() && <button type="button" aria-label={`Remove search filter: ${query}`}
              title="Remove text filter" className={chipClass}
              onClick={() => actions.setGridSearchQuery('')}>
              <span className="min-w-0 break-all">Search: {query} ×</span>
            </button>}
            {provider && <button type="button" aria-label={`Remove provider filter: ${provider}`}
              title="Remove provider filter" className={chipClass}
              onClick={() => actions.setGridProviderFilter(null)}>
              <span className="min-w-0 break-all">Provider: {provider} ×</span>
            </button>}
            {status && <button type="button" aria-label={`Remove status filter: ${STATUS_CONFIG[status].label}`}
              title="Remove status filter" className={chipClass}
              onClick={() => actions.setGridStatusFilter(null)}>
              <span className="min-w-0 break-all">Status: {STATUS_CONFIG[status].label} ×</span>
            </button>}
          </div>
          <button type="button" onClick={actions.resetGridControls} className="rounded-md border border-border-default px-3 py-1.5 text-text-primary hover:bg-bg-card-hover">Clear all</button>
        </div>
      )}
    </div>
  );
}
