/**
 * PROTOTYPE - issue #1375, second navigation exploration.
 *
 * Five title-bar-first models for Probe, switchable with `?variant=A|B|C|D|E`.
 * The closed state deliberately renders no Probe rail. Search controls use
 * the real command omnibar, while Usage has a direct title-bar affordance in
 * every model so its daily-glance use case stays testable.
 */

import { useEffect, useRef, useState, type MouseEvent, type MutableRefObject, type ReactNode } from 'react';
import { useUIStore, type OmnibarMode, type ProbeTab } from '../../stores/uiStore';
import { useProbeContext } from '../../hooks/useProbeContext';
import type { ProbeContext, ProbeTabDefinition } from '../../lib/probeContext';
import { useProbeResize, PROBE_PANEL_BOUNDS } from './useProbeResize';
import { SearchIcon, type ProbeIcon } from './probeIcons';

export type ProbePrototypeVariant = 'A' | 'B' | 'C' | 'D' | 'E';

export interface ProbePrototypeTab extends ProbeTabDefinition {
  tab: ProbeTab;
  icon: ProbeIcon;
}

interface ProbePanelPrototypeProps {
  variant: ProbePrototypeVariant;
  tabs: readonly ProbePrototypeTab[];
  renderTab: (tab: ProbeTab) => ReactNode;
}

const TASK_LABELS: Record<ProbeTab, string> = {
  files: 'Files',
  review: 'Changes',
  usage: 'Usage',
  worktrees: 'Worktrees',
  properties: 'Project settings',
  autopilot: 'Autopilot',
  circuits: 'Circuits',
  issues: 'Issues',
  pulls: 'Pull requests',
  sessions: 'History',
  scratchpad: 'Scratch pad',
};

const TASK_DESCRIPTIONS: Record<ProbeTab, string> = {
  files: 'Browse the project and open a file',
  review: 'Review what your agent changed',
  usage: 'Check provider usage and limits',
  worktrees: 'Switch branches and working folders',
  properties: 'Configure this project',
  autopilot: 'Keep recurring work moving',
  circuits: 'Inspect connected automations',
  issues: 'Find work to pick up',
  pulls: 'See what is ready to merge',
  sessions: 'Return to previous agent sessions',
  scratchpad: 'Keep notes beside the work',
};

const VARIANTS: readonly { id: ProbePrototypeVariant; label: string; hint: string }[] = [
  { id: 'A', label: 'Title-bar search', hint: 'universal entry point plus Usage' },
  { id: 'B', label: 'Usage status', hint: 'daily glance gets first-class status' },
  { id: 'C', label: 'Context actions', hint: 'choose an action for this thing' },
  { id: 'D', label: 'Peek card', hint: 'Probe floats in only when needed' },
  { id: 'E', label: 'Command palette', hint: 'search is the navigation model' },
];

export function getProbePrototypeVariant(): ProbePrototypeVariant | null {
  if (!import.meta.env.DEV || typeof window === 'undefined') return null;
  const value = new URLSearchParams(window.location.search).get('variant');
  return VARIANTS.some((candidate) => candidate.id === value)
    ? value as ProbePrototypeVariant
    : null;
}

/** Title-bar controls are part of the prototype because the closed state has no Probe affordance. */
export function ProbeTitleBarPrototype({ variant }: { variant: ProbePrototypeVariant }) {
  const openOmnibar = useUIStore((state) => state.openOmnibar);
  const openProbeTab = useUIStore((state) => state.openProbeTab);
  const openSearch = (mode: OmnibarMode = 'files') => openOmnibar(mode);
  const openUsage = () => openProbeTab('usage');

  return (
    <div className="flex min-w-0 shrink-0 items-center gap-1.5 px-2">
      {variant === 'A' && (
        <>
          <TitleBarSearch label="Search everything..." shortcut="Ctrl K" onClick={() => openSearch('files')} />
          <UsageTitleBarButton onClick={openUsage} />
        </>
      )}
      {variant === 'B' && (
        <>
          <UsageTitleBarButton onClick={openUsage} prominent />
          <TitleBarActionButton label="Open a view..." onClick={() => openSearch('files')} />
        </>
      )}
      {variant === 'C' && (
        <>
          <TitleBarContextButton onClick={() => openProbeTab('review')} />
          <UsageTitleBarButton onClick={openUsage} />
        </>
      )}
      {variant === 'D' && (
        <>
          <TitleBarActionButton label="Inspect or open..." onClick={() => openSearch('files')} wide />
          <UsageTitleBarButton onClick={openUsage} />
        </>
      )}
      {variant === 'E' && (
        <>
          <TitleBarSearch label="Search views, commands, nodes..." shortcut="Ctrl K" onClick={() => openSearch('files')} wide />
          <UsageTitleBarButton onClick={openUsage} />
        </>
      )}
    </div>
  );
}

function TitleBarSearch({ label, shortcut, onClick, wide = false }: { label: string; shortcut: string; onClick: () => void; wide?: boolean }) {
  return (
    <button type="button" onClick={onClick} aria-label={label} title="Open command palette" className={`flex h-7 items-center gap-2 rounded-md border border-border-default bg-bg-base px-2.5 text-2xs text-text-muted transition-colors hover:border-accent-cyan/50 hover:text-text-primary ${wide ? 'w-[390px]' : 'w-[330px]'}`}>
      <SearchIcon className="h-3.5 w-3.5 shrink-0" />
      <span className="min-w-0 flex-1 truncate text-left">{label}</span>
      <kbd className="shrink-0 rounded-md border border-border-default bg-bg-card px-1.5 py-0.5 font-mono text-[9px] text-text-muted">{shortcut}</kbd>
    </button>
  );
}

function TitleBarActionButton({ label, onClick, wide = false }: { label: string; onClick: () => void; wide?: boolean }) {
  return (
    <button type="button" onClick={onClick} aria-label={label} className={`flex h-7 items-center gap-2 rounded-md border border-border-default px-2.5 text-2xs text-text-secondary transition-colors hover:border-accent-cyan/50 hover:bg-bg-card hover:text-text-primary ${wide ? 'w-[250px]' : 'w-[145px]'}`}>
      <SearchIcon className="h-3.5 w-3.5 text-text-muted" />
      <span className="truncate">{label}</span>
    </button>
  );
}

function TitleBarContextButton({ onClick }: { onClick: () => void }) {
  return (
    <button type="button" onClick={onClick} aria-label="Open actions for current context" className="flex h-7 max-w-[260px] items-center gap-2 rounded-md border border-accent-cyan/30 bg-accent-cyan/5 px-2.5 text-2xs text-text-secondary transition-colors hover:bg-accent-cyan/10 hover:text-text-primary">
      <span className="h-1.5 w-1.5 shrink-0 rounded-full bg-accent-cyan" />
      <span className="truncate">Current work</span>
      <span className="truncate text-text-muted">- choose an action</span>
    </button>
  );
}

function UsageTitleBarButton({ onClick, prominent = false }: { onClick: () => void; prominent?: boolean }) {
  return <button type="button" onClick={onClick} aria-label="Open Usage" title="Open Usage" className={`flex h-7 items-center gap-1.5 rounded-md border px-2 text-2xs transition-colors ${prominent ? 'border-accent-cyan/40 bg-accent-cyan/10 text-accent-cyan hover:bg-accent-cyan/15' : 'border-border-default text-text-secondary hover:bg-bg-card hover:text-text-primary'}`}><UsageGlyph className="h-3.5 w-3.5" /><span>Usage</span></button>;
}

export function ProbePanelPrototype({ variant, tabs, renderTab }: ProbePanelPrototypeProps) {
  const [currentVariant, setCurrentVariant] = useState(variant);
  const [contextMenuOpen, setContextMenuOpen] = useState(variant === 'C');
  const probeOpen = useUIStore((state) => state.probeOpen);
  const probeTab = useUIStore((state) => state.probeTab);
  const openProbeTab = useUIStore((state) => state.openProbeTab);
  const toggleProbe = useUIStore((state) => state.toggleProbe);
  const openOmnibar = useUIStore((state) => state.openOmnibar);
  const pinProbeContext = useUIStore((state) => state.pinProbeContext);
  const clearProbeContextPin = useUIStore((state) => state.clearProbeContextPin);
  const { width: bodyWidth, isResizing, handleMouseDown, setWidth: setBodyWidth } = useProbeResize();
  const bodyWidthRef = useRef(bodyWidth);
  bodyWidthRef.current = bodyWidth;
  const context = useProbeContext();

  // Unlike the earlier rail prototype, the closed state has no replacement
  // rail. If HMR carried over an open production/prototype panel, start this
  // title-bar experiment in its intended closed state.
  useEffect(() => {
    if (useUIStore.getState().probeOpen) toggleProbe();
  }, [toggleProbe]);

  useEffect(() => {
    setCurrentVariant(variant);
    setContextMenuOpen(variant === 'C');
  }, [variant]);

  const activeDef = tabs.find((tab) => tab.tab === probeTab) ?? tabs[0];
  const selectTab = (tab: ProbeTab) => {
    setContextMenuOpen(false);
    openProbeTab(tab);
  };
  const goHome = () => {
    setContextMenuOpen(currentVariant === 'C');
    if (!useUIStore.getState().probeOpen) openProbeTab(probeTab);
  };
  const handlePinToggle = () => {
    if (context.mode === 'pinned') clearProbeContextPin();
    else if (context.pinCandidate !== null) pinProbeContext(context.pinCandidate);
  };
  const commonProps: VariantProps = {
    tabs,
    probeTab,
    probeOpen,
    context,
    activeDef,
    renderTab,
    onSelectTab: selectTab,
    onPinToggle: handlePinToggle,
    onClose: toggleProbe,
    onHome: goHome,
    onOpenLauncher: () => openOmnibar('files'),
  };

  const body = currentVariant === 'A'
    ? <VariantA {...commonProps} />
    : currentVariant === 'B'
      ? <VariantB {...commonProps} />
      : currentVariant === 'C'
        ? <VariantC {...commonProps} menuOpen={contextMenuOpen} />
        : currentVariant === 'D'
          ? <VariantD {...commonProps} />
          : <VariantE {...commonProps} />;

  return (
    <PrototypeDock body={body} bodyWidth={bodyWidth} isResizing={isResizing} bodyWidthRef={bodyWidthRef} handleMouseDown={handleMouseDown} setWidth={setBodyWidth} probeOpen={probeOpen} variant={currentVariant} onVariantChange={setCurrentVariant} />
  );
}

interface VariantProps {
  tabs: readonly ProbePrototypeTab[];
  probeTab: ProbeTab;
  probeOpen: boolean;
  context: ProbeContext;
  activeDef: ProbePrototypeTab;
  renderTab: (tab: ProbeTab) => ReactNode;
  onSelectTab: (tab: ProbeTab) => void;
  onPinToggle: () => void;
  onClose: () => void;
  onHome: () => void;
  onOpenLauncher: () => void;
}

function VariantA(props: VariantProps) {
  return <DetailView {...props} eyebrow="TITLE-BAR SEARCH" title={taskLabel(props.activeDef.tab)} headerAction={<ViewChooserButton onClick={props.onOpenLauncher} label="Change view" />} />;
}

function VariantB(props: VariantProps) {
  return (
    <section className="flex h-full min-h-0 flex-col bg-bg-surface">
      <DetailHeader {...props} eyebrow="USAGE STATUS" title={taskLabel(props.activeDef.tab)} />
      <UsageSummary onOpenUsage={() => props.onSelectTab('usage')} />
      <ActiveContextLine context={props.context} activeDef={props.activeDef} />
      <ActiveTabContent tab={props.probeTab} label={taskLabel(props.activeDef.tab)} renderTab={props.renderTab} />
    </section>
  );
}

function VariantC(props: VariantProps & { menuOpen: boolean }) {
  return props.menuOpen
    ? <ContextActionMenu {...props} />
    : <DetailView {...props} eyebrow="CURRENT WORK" title={taskLabel(props.activeDef.tab)} headerAction={<HomeButton onClick={props.onHome} />} />;
}

function VariantD(props: VariantProps) {
  return (
    <div className="h-full bg-bg-base p-2">
      <section className="flex h-full min-h-0 flex-col overflow-hidden rounded-lg border border-border-default bg-bg-surface shadow-lg">
        <DetailHeader {...props} eyebrow="PEEK" title={taskLabel(props.activeDef.tab)} />
        <div className="flex shrink-0 items-center justify-between border-b border-border-subtle bg-bg-card/50 px-3 py-2">
          <div className="text-2xs text-text-secondary">Opened over the workspace</div>
          <ViewChooserButton onClick={props.onOpenLauncher} label="Switch" />
        </div>
        <ActiveContextLine context={props.context} activeDef={props.activeDef} />
        <ActiveTabContent tab={props.probeTab} label={taskLabel(props.activeDef.tab)} renderTab={props.renderTab} />
      </section>
    </div>
  );
}

function VariantE(props: VariantProps) {
  return (
    <section className="flex h-full min-h-0 flex-col bg-bg-surface">
      <DetailHeader {...props} eyebrow="COMMAND PALETTE" title={taskLabel(props.activeDef.tab)} headerAction={<ViewChooserButton onClick={props.onOpenLauncher} label="Search" />} />
      <div className="flex shrink-0 items-center gap-2 border-b border-border-subtle bg-accent-cyan/5 px-3 py-2 text-2xs text-text-secondary">
        <SearchIcon className="h-3.5 w-3.5 text-accent-cyan" />
        <span>Opened from the title-bar command field</span>
        <kbd className="ml-auto rounded-md border border-border-default bg-bg-card px-1.5 py-0.5 font-mono text-[9px] text-text-muted">Ctrl K</kbd>
      </div>
      <ActiveContextLine context={props.context} activeDef={props.activeDef} />
      <ActiveTabContent tab={props.probeTab} label={taskLabel(props.activeDef.tab)} renderTab={props.renderTab} />
    </section>
  );
}

function ContextActionMenu(props: VariantProps) {
  const actionTabs = props.context.lens === 'host' ? ['usage'] as const : props.context.lens === 'agent' ? ['review', 'files', 'sessions', 'pulls'] as const : ['files', 'pulls', 'issues', 'scratchpad'] as const;
  return (
    <section className="flex h-full min-h-0 flex-col bg-bg-surface">
      <DetailHeader {...props} eyebrow="CURRENT WORK" title={props.context.subjectLabel} />
      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-3">
        <div className="rounded-lg border border-accent-cyan/30 bg-accent-cyan/5 px-3 py-2.5">
          <div className="flex items-center gap-2">
            <span className="h-2 w-2 rounded-full bg-accent-cyan" />
            <span className="truncate text-xs font-medium text-text-primary">{props.context.subjectLabel}</span>
          </div>
          <div className="mt-1 text-2xs text-text-secondary">What do you want to do with this?</div>
        </div>
        <div className="mt-4 flex flex-col gap-1.5">
          {getDestinations(props.tabs, actionTabs).map((destination) => (
            <button key={destination.definition.tab} type="button" onClick={() => props.onSelectTab(destination.definition.tab)} className="flex items-center gap-2 rounded-md border border-border-default bg-bg-card px-2.5 py-2 text-left transition-colors hover:border-accent-cyan/40">
              <destination.definition.icon className="h-4 w-4 shrink-0 text-accent-cyan" />
              <span className="min-w-0 flex-1">
                <span className="block truncate text-xs font-medium text-text-primary">{destination.label}</span>
                <span className="mt-0.5 block truncate text-2xs text-text-muted">{destination.description}</span>
              </span>
              <ChevronIcon className="h-3.5 w-3.5 text-text-muted" />
            </button>
          ))}
        </div>
      </div>
    </section>
  );
}

function UsageSummary({ onOpenUsage }: { onOpenUsage: () => void }) {
  return (
    <div className="shrink-0 border-b border-border-subtle bg-accent-cyan/5 px-3 py-2.5">
      <button type="button" onClick={onOpenUsage} className="flex w-full items-center gap-2 text-left">
        <span className="flex h-7 w-7 shrink-0 items-center justify-center rounded-md bg-accent-cyan/10 text-accent-cyan"><UsageGlyph className="h-4 w-4" /></span>
        <span className="min-w-0 flex-1">
          <span className="block text-xs font-medium text-text-primary">Provider usage</span>
          <span className="mt-0.5 block truncate text-2xs text-text-muted">Open to refresh provider meters and limits</span>
        </span>
      </button>
    </div>
  );
}

function DetailView(props: VariantProps & { eyebrow: string; title: string; headerAction?: ReactNode }) {
  return (
    <section className="flex h-full min-h-0 flex-col bg-bg-surface">
      <DetailHeader {...props} eyebrow={props.eyebrow} title={props.title} headerAction={props.headerAction} />
      <ActiveContextLine context={props.context} activeDef={props.activeDef} />
      <ActiveTabContent tab={props.probeTab} label={taskLabel(props.activeDef.tab)} renderTab={props.renderTab} />
    </section>
  );
}

function DetailHeader({ eyebrow, title, headerAction, ...props }: VariantProps & { eyebrow: string; title: string; headerAction?: ReactNode }) {
  return (
    <header className="flex shrink-0 items-start gap-2 border-b border-border-subtle bg-bg-surface px-3 pb-2 pt-3">
      <div className="min-w-0 flex-1">
        <div className="text-2xs font-semibold uppercase tracking-[0.14em] text-text-muted">{eyebrow}</div>
        <h2 className="mt-1 truncate text-sm font-medium text-text-primary">{title}</h2>
        <div className="mt-1 flex min-w-0 items-center gap-1 text-xs text-text-secondary" title={props.context.subjectLabel}>
          <span className="truncate">{props.context.subjectLabel}</span>
          <span className="shrink-0 text-text-muted">-</span>
          <span className="truncate text-text-muted">{contextModeLabel(props.context)}</span>
        </div>
      </div>
      <div className="flex shrink-0 items-center gap-1">
        {headerAction}
        <PanelControls {...props} />
      </div>
    </header>
  );
}

function PanelControls({ context, onPinToggle, onClose }: VariantProps) {
  return (
    <>
      {context.canPin && <button type="button" onClick={onPinToggle} aria-pressed={context.mode === 'pinned'} aria-label={context.mode === 'pinned' ? 'Unpin context' : 'Pin context'} className={`rounded-md p-1.5 transition-colors ${context.mode === 'pinned' ? 'bg-accent-cyan/10 text-accent-cyan' : 'text-text-muted hover:bg-bg-card hover:text-text-primary'}`}><PinIcon className="h-3.5 w-3.5" /></button>}
      <button type="button" onClick={onClose} aria-label="Close panel" title="Close panel" className="rounded-md p-1.5 text-text-muted transition-colors hover:bg-bg-card hover:text-text-primary"><CloseIcon className="h-3.5 w-3.5" /></button>
    </>
  );
}

function ViewChooserButton({ onClick, label }: { onClick: () => void; label: string }) {
  return <button type="button" onClick={onClick} aria-label={label} className="rounded-md border border-border-default px-2 py-1 text-2xs text-text-secondary transition-colors hover:bg-bg-card hover:text-text-primary">{label}</button>;
}

function HomeButton({ onClick }: { onClick: () => void }) {
  return <button type="button" onClick={onClick} aria-label="Back to Probe home" title="Back to Probe home" className="rounded-md p-1.5 text-text-muted transition-colors hover:bg-bg-card hover:text-text-primary"><HomeIcon className="h-3.5 w-3.5" /></button>;
}

function ActiveContextLine({ context, activeDef }: { context: ProbeContext; activeDef: ProbePrototypeTab }) {
  return <div className="flex shrink-0 items-center gap-2 border-b border-border-subtle px-3 py-2 text-2xs text-text-muted"><span className="truncate">{taskLabel(activeDef.tab)}</span><span className="ml-auto truncate">{context.detailLabel ?? contextModeLabel(context)}</span></div>;
}

function ActiveTabContent({ tab, label, renderTab }: { tab: ProbeTab; label: string; renderTab: (tab: ProbeTab) => ReactNode }) {
  return <div id="probe-layout-panel" role="region" aria-label={`${label} content`} className="flex min-h-0 flex-1 flex-col overflow-hidden"><div key={tab} className="flex min-h-0 flex-1 flex-col animate-fade-in">{renderTab(tab)}</div></div>;
}

interface PrototypeDockProps {
  body: ReactNode;
  bodyWidth: number;
  isResizing: boolean;
  bodyWidthRef: MutableRefObject<number>;
  handleMouseDown: (event: MouseEvent<HTMLDivElement>) => void;
  setWidth: (next: number) => void;
  probeOpen: boolean;
  variant: ProbePrototypeVariant;
  onVariantChange: (variant: ProbePrototypeVariant) => void;
}

function PrototypeDock({ body, bodyWidth, isResizing, bodyWidthRef, handleMouseDown, setWidth, probeOpen, variant, onVariantChange }: PrototypeDockProps) {
  return (
    <div className="relative h-full shrink-0 bg-transparent">
      {probeOpen && <div className="relative h-full shrink-0 animate-slide-in-right" style={{ width: bodyWidth }}><ResizeHandle bodyWidth={bodyWidth} bodyWidthRef={bodyWidthRef} isResizing={isResizing} handleMouseDown={handleMouseDown} setWidth={setWidth} /><section role="region" aria-label="Probe title-bar prototype" className="h-full w-full min-w-0 overflow-hidden border-l border-border-subtle">{body}</section></div>}
      <PrototypeSwitcher current={variant} onChange={onVariantChange} />
    </div>
  );
}

function ResizeHandle({ bodyWidth, bodyWidthRef, isResizing, handleMouseDown, setWidth }: { bodyWidth: number; bodyWidthRef: MutableRefObject<number>; isResizing: boolean; handleMouseDown: (event: MouseEvent<HTMLDivElement>) => void; setWidth: (next: number) => void }) {
  return <div onMouseDown={handleMouseDown} role="separator" aria-orientation="vertical" aria-label="Resize probe panel" aria-valuenow={bodyWidth} aria-valuemin={PROBE_PANEL_BOUNDS.MIN_WIDTH} aria-valuemax={PROBE_PANEL_BOUNDS.MAX_WIDTH} tabIndex={0} onKeyDown={(event) => { const current = bodyWidthRef.current; const min = PROBE_PANEL_BOUNDS.MIN_WIDTH; const max = PROBE_PANEL_BOUNDS.MAX_WIDTH; const small = event.shiftKey ? 32 : 8; let next: number | null = null; if (event.key === 'ArrowLeft') next = Math.min(max, current + small); if (event.key === 'ArrowRight') next = Math.max(min, current - small); if (event.key === 'Home') next = max; if (event.key === 'End') next = min; if (next !== null) { setWidth(next); event.preventDefault(); } }} className={`absolute -left-1 top-0 z-10 h-full w-2.5 cursor-col-resize outline-none after:absolute after:inset-y-0 after:left-1 after:w-0.5 after:transition-colors focus-visible:outline-2 focus-visible:outline-accent-cyan ${isResizing ? 'after:bg-accent-cyan/60' : 'after:bg-transparent hover:after:bg-accent-cyan/40'}`} />;
}

function PrototypeSwitcher({ current, onChange }: { current: ProbePrototypeVariant; onChange: (variant: ProbePrototypeVariant) => void }) {
  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      const target = event.target as HTMLElement | null;
      if (target?.matches('input, textarea, select, [contenteditable="true"]')) return;
      if (event.key !== 'ArrowLeft' && event.key !== 'ArrowRight') return;
      const index = VARIANTS.findIndex((candidate) => candidate.id === current);
      const delta = event.key === 'ArrowRight' ? 1 : -1;
      const next = VARIANTS[(index + delta + VARIANTS.length) % VARIANTS.length].id;
      changeVariantInUrl(next);
      onChange(next);
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [current, onChange]);

  const changeVariant = (next: ProbePrototypeVariant) => { changeVariantInUrl(next); onChange(next); };
  const index = VARIANTS.findIndex((candidate) => candidate.id === current);
  const previous = VARIANTS[(index - 1 + VARIANTS.length) % VARIANTS.length];
  const next = VARIANTS[(index + 1) % VARIANTS.length];
  const metadata = VARIANTS[index];
  return <div className="fixed bottom-4 left-1/2 z-[80] flex -translate-x-1/2 items-center gap-2 rounded-full border border-accent-cyan/40 bg-bg-overlay px-2 py-1.5 text-xs text-text-primary shadow-md" data-testid="probe-prototype-switcher"><button type="button" onClick={() => changeVariant(previous.id)} aria-label={`Previous prototype: ${previous.label}`} className="rounded-full px-2 py-1 text-text-secondary hover:bg-bg-card hover:text-text-primary">&lt;</button><div className="min-w-[230px] text-center leading-tight"><div className="font-medium text-accent-cyan">Prototype {current} - {metadata.label}</div><div className="text-2xs text-text-muted">{metadata.hint} - arrow keys to compare</div></div><button type="button" onClick={() => changeVariant(next.id)} aria-label={`Next prototype: ${next.label}`} className="rounded-full px-2 py-1 text-text-secondary hover:bg-bg-card hover:text-text-primary">&gt;</button></div>;
}

function changeVariantInUrl(variant: ProbePrototypeVariant) {
  const url = new URL(window.location.href);
  url.searchParams.set('variant', variant);
  window.history.replaceState({}, '', url);
}

function getDestination(tabs: readonly ProbePrototypeTab[], tab: ProbeTab): Destination | null {
  const definition = tabs.find((candidate) => candidate.tab === tab);
  return definition ? { definition, label: taskLabel(tab), description: TASK_DESCRIPTIONS[tab] } : null;
}

function getDestinations(tabs: readonly ProbePrototypeTab[], order: readonly ProbeTab[]): Destination[] {
  return order.flatMap((tab) => { const destination = getDestination(tabs, tab); return destination ? [destination] : []; });
}

interface Destination {
  definition: ProbePrototypeTab;
  label: string;
  description: string;
}

function taskLabel(tab: ProbeTab): string { return TASK_LABELS[tab]; }

function contextModeLabel(context: ProbeContext): string {
  if (context.mode === 'pinned') return 'Pinned context';
  if (context.lens === 'host') return 'App-wide';
  if (context.followsSelection) return 'Following selection';
  return 'Fixed context';
}

function UsageGlyph({ className }: { className?: string }) {
  return <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="M4 19a8 8 0 1 1 16 0" /><path d="M12 11v4" /><path d="M12 19h.01" /></svg>;
}

function PinIcon({ className }: { className?: string }) {
  return <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="m12 17 5-5" /><path d="M9 3h6l1 5-4 4v5l-2 2v-7L6 8l1-5Z" /><path d="M5 21h14" /></svg>;
}

function CloseIcon({ className }: { className?: string }) {
  return <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><rect width="18" height="18" x="3" y="3" rx="2" /><path d="M15 3v18" /><path d="m10 9-3 3 3 3" /></svg>;
}

function HomeIcon({ className }: { className?: string }) {
  return <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="m3 10 9-7 9 7" /><path d="M5 9v11h14V9" /><path d="M9 20v-6h6v6" /></svg>;
}

function ChevronIcon({ className }: { className?: string }) {
  return <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true"><path d="m9 18 6-6-6-6" /></svg>;
}
