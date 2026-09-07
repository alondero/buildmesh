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
  const tabRefs = useRef<(HTMLButtonElement | null)[]>([]);
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
  useAriaMenu({ rootRef: menuRef, itemCount: tabs.length, activeIndex, setActiveIndex,
    onClose: closeMenu, enabled: open });
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
          return <div key={tab.key} role="presentation"
            className={`flex shrink-0 items-center border-b-2 ${selected ? 'border-accent-cyan bg-accent-cyan/5' : 'border-transparent'}`}>
            <button ref={el => { tabRefs.current[index] = el; }} type="button" role="tab"
              id={`activity-${rootId}-${tab.key}`} aria-controls={`activity-panel-${rootId}`}
              aria-label={fullLabel(tab)} title={fullLabel(tab)} aria-selected={selected} tabIndex={selected ? 0 : -1}
              onClick={event => { event.stopPropagation(); onSelect(tab.member.id, tab.utility); }}
              onKeyDown={event => {
                const target = event.key === 'ArrowRight' ? (index + 1) % tabs.length
                  : event.key === 'ArrowLeft' ? (index + tabs.length - 1) % tabs.length
                  : event.key === 'Home' ? 0 : event.key === 'End' ? tabs.length - 1 : null;
                if (target === null) return;
                event.preventDefault(); event.stopPropagation();
                onSelect(tabs[target].member.id, tabs[target].utility, false);
                tabRefs.current[target]?.focus({ preventScroll: true });
              }}
              className={`flex h-8 max-w-48 items-center gap-1.5 px-2.5 text-xs transition-colors ${selected ? 'text-text-primary' : 'text-text-muted hover:text-text-primary hover:bg-bg-card'}`}>
              {!tab.utility && <ProviderIcon providerId={tab.member.provider} className="h-3 w-3 shrink-0" />}
              <span className="truncate">{tab.label}</span>
              {!tab.utility && <span aria-hidden="true" className={`shrink-0 text-2xs ${getStatusConfig(tab.member.status).color}`}>{statusGlyph(tab.member.status)}</span>}
            </button>
            {tab.utility && <button type="button" aria-label={`Close ${fullLabel(tab)}`} title={`Close ${fullLabel(tab)}`}
              onClick={event => { event.stopPropagation(); onClose(tab.member.id); }}
              className="mr-1 flex h-6 w-6 shrink-0 items-center justify-center rounded-sm text-text-muted hover:bg-status-error-bg hover:text-status-error">×</button>}
          </div>;
        })}
      </div>
      <button ref={triggerRef} type="button" data-dropdown-for={menuId} aria-label={`All sessions (${tabs.length})`} title="All sessions"
        aria-haspopup="menu" aria-expanded={open} aria-controls={open ? menuId : undefined}
        onClick={event => { event.stopPropagation(); setActiveIndex(Math.max(0, selectedIndex)); setOpen(!open); }}
        className="flex w-8 shrink-0 items-center justify-center border-l border-border-subtle text-text-muted hover:bg-bg-card hover:text-text-primary">
        <svg aria-hidden="true" width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><path d="m6 9 6 6 6-6" /></svg>
      </button>
      {open && createPortal(<div ref={menuRef} id={menuId} data-dropdown-for={menuId} role="menu" aria-label="All sessions"
        className="fixed z-[100] w-64 max-w-[calc(100vw-16px)] max-h-80 overflow-y-auto rounded-md border border-border-default bg-bg-overlay p-1 shadow-md"
        style={{ top: 0, left: 0 }}>
        {tabs.map((tab, index) => <button key={tab.key} type="button" role="menuitem" tabIndex={index === activeIndex ? 0 : -1}
          aria-label={fullLabel(tab)} aria-current={index === selectedIndex ? 'true' : undefined}
          onClick={event => {
            event.stopPropagation();
            setOpen(false);
            // Native button activation reports detail=0 for keyboard
            // activation and a positive detail for pointer clicks. Pointer
            // selection should move focus into the terminal; keyboard
            // selection keeps the roving tab focus on the selected tab.
            const focusTerminal = event.detail > 0;
            onSelect(tab.member.id, tab.utility, focusTerminal);
            if (!focusTerminal) {
              requestAnimationFrame(() => tabRefs.current[index]?.focus({ preventScroll: true }));
            }
          }}
          className="flex w-full items-center gap-2 rounded-sm px-2 py-2 text-left text-xs text-text-secondary hover:bg-bg-card focus:bg-bg-card">
          <span aria-hidden="true" className={tab.utility ? 'text-text-muted' : getStatusConfig(tab.member.status).color}>{index === selectedIndex ? '✓' : tab.utility ? '›' : statusGlyph(tab.member.status)}</span>
          <span className="min-w-0 flex-1"><span className="block font-medium text-text-primary">{tab.label}</span><span className="block truncate text-text-muted">{tab.member.name}</span></span>
          {!tab.utility && <span className="text-2xs text-text-muted">{tab.member.status.replace(/_/g, ' ')}</span>}
        </button>)}
      </div>, document.body)}
    </div>
  );
}
