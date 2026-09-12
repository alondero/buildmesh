import { useEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import type { AgentNode } from '../../stores/agentNodeStore';
import type { UtilityMode } from '../../stores/nodeActivityStore';
import { getStatusConfig } from '../../lib/status';
import { useAriaMenu } from '../../hooks/useAriaMenu';
import { useClickOutside } from '../../hooks/useClickOutside';
import { useAnchoredPosition } from '../../hooks/useAnchoredPosition';
import { ProviderIcon } from '../Providers/ProviderIcon';

interface NodeActivityTabsProps {
  rootId: number;
  members: readonly AgentNode[];
  utilities: ReadonlyMap<number, UtilityMode | undefined>;
  selectedId: number;
  showingUtility: boolean;
  onSelect: (id: number, utility?: boolean, focusTerminal?: boolean) => void;
  onClose: (id: number) => void;
}

export function NodeActivityTabs({ rootId, members, utilities, selectedId, showingUtility, onSelect, onClose }: NodeActivityTabsProps) {
  const [open, setOpen] = useState(false);
  const [activeIndex, setActiveIndex] = useState(0);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const menuRef = useRef<HTMLDivElement>(null);
  const tabRefs = useRef<(HTMLDivElement | null)[]>([]);
  // A click event alone cannot reliably distinguish pointer activation from
  // keyboard or assistive-technology activation. Pointer events are explicit
  // for mouse and touch; keyboard activation leaves this at the safe default.
  const activationRef = useRef<'pointer' | 'keyboard'>('keyboard');
  const menuId = `activity-list-${rootId}`;
  const tabs = members.flatMap(member => {
    const role = member.id === rootId ? (members.length > 1 ? 'Implementation' : 'Agent')
      : members.length > 2 ? `Review ${members.filter(n => n.id !== rootId).findIndex(n => n.id === member.id) + 1}` : 'Review';
    const agent = { key: `agent-${member.id}`, member, utility: false, label: role };
    const mode = utilities.get(member.id);
    return mode ? [agent, { key: `utility-${member.id}`, member, utility: true,
      label: `${mode[0].toUpperCase()}${mode.slice(1)}${members.filter(n => utilities.get(n.id)).length > 1 ? ` · ${role}` : ''}` }] : [agent];
  });
  const selectedIndex = tabs.findIndex(tab => tab.member.id === selectedId && tab.utility === showingUtility);
  const closeMenu = () => { setOpen(false); triggerRef.current?.focus({ preventScroll: true }); };
  useClickOutside(open ? menuId : null, () => setOpen(false));
  useAnchoredPosition(triggerRef, menuRef, open, { align: 'end' });
  // Issue #1720 follow-up — single-caret menu. Rows paint only off
  // `activeIndex` (no CSS hover paint); pointer entry focuses the
  // entered row and its focus handler syncs the index, so hover moves
  // the caret instead of lighting a second row. The hook seeds the
  // open caret at the selected session via `initialActiveIndex` (the
  // mount effect would otherwise reset it to row 0 and clobber the
  // trigger's pre-open `setActiveIndex`). The strip's committed
  // selection stays marked by the ✓ glyph / `aria-current` — data,
  // not a competing highlight.
  useAriaMenu({ rootRef: menuRef, activeIndex, setActiveIndex,
    onClose: closeMenu, enabled: open, initialActiveIndex: Math.max(0, selectedIndex) });
  useEffect(() => {
    tabRefs.current[selectedIndex]?.scrollIntoView?.({ block: 'nearest', inline: 'nearest' });
  }, [selectedIndex]);
  const fullLabel = (tab: typeof tabs[number]) => `${tab.label} · ${tab.member.name}${tab.utility ? '' : ` · ${tab.member.status.replace(/_/g, ' ')}`}`;
  const statusGlyph = (status: string) => status === 'awaiting_input' ? '!' : status === 'error' ? '×'
    : status === 'completed' || status === 'ready' ? '✓' : status === 'suspended' ? 'Ⅱ' : '●';

  return (
    <div className="flex shrink-0 min-w-0 border-b border-border-default bg-bg-base/60">
      <div role="tablist" aria-label="Node activities" className="flex min-w-0 flex-1 overflow-x-auto">
        {tabs.map((tab, index) => {
          const selected = index === selectedIndex;
          return <div key={tab.key} ref={el => { tabRefs.current[index] = el; }} role="tab"
              id={`activity-${rootId}-${tab.key}`} aria-controls={`activity-panel-${rootId}`}
              aria-label={fullLabel(tab)} title={fullLabel(tab)} aria-selected={selected} tabIndex={selected ? 0 : -1}
              onPointerDown={() => { activationRef.current = 'pointer'; }}
              onClick={event => {
                event.stopPropagation();
                const focusTerminal = activationRef.current === 'pointer';
                activationRef.current = 'keyboard';
                onSelect(tab.member.id, tab.utility, focusTerminal);
              }}
              onKeyDown={event => {
                if (event.key === 'Enter' || event.key === ' ') {
                  event.preventDefault();
                  activationRef.current = 'keyboard';
                  onSelect(tab.member.id, tab.utility, false);
                  return;
                }
                if ((event.key === 'Delete' || event.key === 'Backspace') && selected && tab.utility) {
                  event.preventDefault();
                  onClose(tab.member.id);
                  return;
                }
                const target = event.key === 'ArrowRight' ? (index + 1) % tabs.length
                  : event.key === 'ArrowLeft' ? (index + tabs.length - 1) % tabs.length
                  : event.key === 'Home' ? 0 : event.key === 'End' ? tabs.length - 1 : null;
                if (target === null) return;
                event.preventDefault(); event.stopPropagation();
                onSelect(tabs[target].member.id, tabs[target].utility, false);
                tabRefs.current[target]?.focus({ preventScroll: true });
              }}
              className={`flex h-8 shrink-0 max-w-48 items-center gap-1.5 border-b-2 px-2.5 text-xs transition-colors ${selected ? 'border-accent-cyan bg-accent-cyan/5 text-text-primary' : 'border-transparent text-text-muted hover:bg-bg-card hover:text-text-primary'}`}>
              {!tab.utility && <ProviderIcon providerId={tab.member.provider} className="h-3 w-3 shrink-0" />}
              <span className="truncate">{tab.label}</span>
              {!tab.utility && <span aria-hidden="true" className={`shrink-0 text-2xs ${getStatusConfig(tab.member.status).color}`}>{statusGlyph(tab.member.status)}</span>}
              {tab.utility && <button type="button" tabIndex={-1}
                aria-label={`Close ${fullLabel(tab)}`} title={`Close ${fullLabel(tab)}`}
                onClick={event => { event.stopPropagation(); onClose(tab.member.id); }}
                onKeyDown={event => {
                  if (event.key === 'Enter' || event.key === ' ') {
                    event.preventDefault(); event.stopPropagation(); onClose(tab.member.id);
                  }
                }}
                className="ml-auto flex h-5 w-5 shrink-0 items-center justify-center rounded-sm text-text-muted hover:bg-status-error-bg hover:text-status-error">&times;</button>}
            </div>;
        })}
      </div>


      <button ref={triggerRef} type="button" data-dropdown-for={menuId} aria-label={`All sessions (${tabs.length})`} title="All sessions"
        aria-haspopup="menu" aria-expanded={open} aria-controls={open ? menuId : undefined}
        // No `setActiveIndex` here — the hook's mount layout effect seeds
        // the caret at `initialActiveIndex` (= `selectedIndex`) on open;
        // setting it in the click would be clobbered immediately after.
        onClick={event => { event.stopPropagation(); setOpen(!open); }}
        className="flex w-8 shrink-0 items-center justify-center border-l border-border-subtle text-text-muted hover:bg-bg-card hover:text-text-primary">
        <svg aria-hidden="true" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><path d="m6 9 6 6 6-6" /></svg>
      </button>
      {open && createPortal(<div ref={menuRef} id={menuId} data-dropdown-for={menuId} role="menu" aria-label="All sessions"
        className="fixed z-[100] w-64 max-w-[calc(100vw-16px)] max-h-80 overflow-y-auto rounded-md border border-border-default bg-bg-overlay p-1 shadow-md"
        style={{ top: 0, left: 0 }}>
        {tabs.map((tab, index) => <button key={tab.key} type="button" role="menuitem" tabIndex={index === activeIndex ? 0 : -1}
          aria-label={fullLabel(tab)} aria-current={index === selectedIndex ? 'true' : undefined}
          onPointerDown={() => { activationRef.current = 'pointer'; }}
          onKeyDown={event => {
            if (event.key === 'Enter' || event.key === ' ') activationRef.current = 'keyboard';
          }}
          onClick={event => {
            event.stopPropagation();
            setOpen(false);
            const focusTerminal = activationRef.current === 'pointer';
            activationRef.current = 'keyboard';
            onSelect(tab.member.id, tab.utility, focusTerminal);
            if (!focusTerminal) {
              requestAnimationFrame(() => tabRefs.current[index]?.focus({ preventScroll: true }));
            }
          }}
          // Pointer entry joins the keyboard's single caret: focus the
          // row under the cursor; the focus handler syncs the index.
          onMouseEnter={(e) => e.currentTarget.focus({ preventScroll: true })}
          onFocus={() => setActiveIndex(index)}
          className={`flex w-full items-center gap-2 rounded-sm px-2 py-2 text-left text-xs focus:outline-none ${
            index === activeIndex
              ? 'bg-bg-selection text-text-primary'
              : 'text-text-secondary'
          }`}>
          <span aria-hidden="true" className={tab.utility ? 'text-text-muted' : getStatusConfig(tab.member.status).color}>{index === selectedIndex ? '✓' : tab.utility ? '›' : statusGlyph(tab.member.status)}</span>
          <span className="min-w-0 flex-1"><span className="block font-medium text-text-primary">{tab.label}</span><span className="block truncate text-text-muted">{tab.member.name}</span></span>
          {!tab.utility && <span className="text-2xs text-text-muted">{tab.member.status.replace(/_/g, ' ')}</span>}
        </button>)}
      </div>, document.body)}
    </div>
  );
}
