import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import {
  DndContext,
  KeyboardSensor,
  PointerSensor,
  useSensor,
  useSensors,
  type DragEndEvent,
} from '@dnd-kit/core';
import {
  SortableContext,
  sortableKeyboardCoordinates,
  useSortable,
  verticalListSortingStrategy,
} from '@dnd-kit/sortable';
import { CSS } from '@dnd-kit/utilities';
import type { SpawnOption } from '../../lib/groups';
import type { SpawnConfiguration } from '../../types/generated/SpawnConfiguration';
import { deleteSpawnConfiguration, getLaunchTargets, listProviders, listSpawnConfigurations, reorderSpawnConfigurations, saveSpawnConfiguration, verifyLaunchConfiguration } from '../../lib/tauri/provider';
import { reorderConfigurationIds } from '../../lib/launchConfigurationOrder';
import { LaunchConfigurationEditor, cloneConfiguration } from './LaunchConfigurationEditor';
import type { ProviderPairing } from '../../types/generated/ProviderPairing';
import type { LaunchTarget } from '../../types/generated/LaunchTarget';
import { placeFlyout } from './flyoutPlacement';

function belongsToHarness(configuration: SpawnConfiguration, harnessId: string): boolean {
  return (configuration.harness_id ?? configuration.spawn_option_id.split(':')[0]) === harnessId;
}

import { offeredUnderLogin } from '../../lib/launchSelection';
export { offeredUnderLogin } from '../../lib/launchSelection';

function SortableMenuConfigurationRow({
  value,
  unavailable,
  canReorder,
  activeMenuIndex,
  configurationIndex,
  editIndex,
  cloneIndex,
  reorderIndex,
  menuClass,
  rowActionClass,
  onSelect,
  onEdit,
  onClone,
}: {
  value: SpawnConfiguration;
  unavailable?: string;
  canReorder: boolean;
  activeMenuIndex: number;
  configurationIndex: number | undefined;
  editIndex: number | undefined;
  cloneIndex: number | undefined;
  reorderIndex: number | undefined;
  menuClass: string;
  rowActionClass: string;
  onSelect: (altKey: boolean) => void;
  onEdit: () => void;
  onClone: () => void;
}) {
  const { setNodeRef, transform, transition, isDragging, attributes, listeners } = useSortable({
    id: value.id,
    disabled: !canReorder,
  });
  const style: React.CSSProperties = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.5 : 1,
  };

  return (
    <div ref={setNodeRef} style={style} role="presentation" className="flex items-stretch">
      <button
        type="button"
        role="menuitem"
        data-menu-index={configurationIndex}
        tabIndex={activeMenuIndex === configurationIndex ? 0 : -1}
        aria-disabled={Boolean(unavailable)}
        title={unavailable ?? undefined}
        className={`${menuClass} min-w-0 flex-1 break-words ${unavailable ? 'cursor-default text-text-muted' : ''}`}
        onClick={(e) => { if (!unavailable) onSelect(e.altKey); }}
      >
        {value.name}{unavailable && <span className="block text-text-muted">{unavailable}</span>}
      </button>
      <button
        type="button"
        role="menuitem"
        data-menu-index={editIndex}
        tabIndex={activeMenuIndex === editIndex ? 0 : -1}
        aria-label={`Edit ${value.name}`}
        className={rowActionClass}
        onClick={onEdit}
      >
        Edit
      </button>
      <button
        type="button"
        role="menuitem"
        data-menu-index={cloneIndex}
        tabIndex={activeMenuIndex === cloneIndex ? 0 : -1}
        aria-label={`Clone ${value.name}`}
        className={rowActionClass}
        onClick={onClone}
      >
        Clone
      </button>
      {canReorder && (
        <span
          {...attributes}
          {...listeners}
          role="menuitem"
          data-menu-index={reorderIndex}
          tabIndex={activeMenuIndex === reorderIndex ? 0 : -1}
          aria-roledescription="sortable"
          aria-label={`Reorder ${value.name}`}
          title="Drag to reorder"
          className="order-first inline-flex min-h-[24px] min-w-[24px] shrink-0 items-center justify-center text-text-muted hover:text-text-secondary hover:bg-bg-selection cursor-grab active:cursor-grabbing text-2xs select-none touch-none focus:bg-bg-selection focus:outline-none"
        >
          ⋮⋮
        </span>
      )}
    </div>
  );
}

export function SpawnConfigurationMenu({ option, anchor, keyboard, configurationRows, decorate, login, onSelect, onClose, onDismiss, onEditingChange }: {
  option: SpawnOption;
  anchor: HTMLElement;
  keyboard: boolean;
  configurationRows: SpawnOption[];
  decorate?: (option: SpawnOption) => SpawnOption;
  /** The Harness Login this submenu launches with (opened from the login
   *  submenu). Absent when the harness has a single login. */
  login?: { id: string; label: string };
  onSelect: (providerId: string, altKey: boolean, configurationId?: string, loginId?: string) => void;
  onClose: () => void;
  onDismiss: () => void;
  onEditingChange: (editing: boolean) => void;
}) {
  const [configurations, setConfigurations] = useState<SpawnConfiguration[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [targetError, setTargetError] = useState<string | null>(null);
  const [draft, setDraft] = useState<SpawnConfiguration | null>(option.configuration ?? null);
  const [targets, setTargets] = useState<LaunchTarget[]>([]);
  const [activeMenuIndex, setActiveMenuIndex] = useState(0);
  const [dragging, setDragging] = useState(false);
  const panel = useRef<HTMLDivElement>(null);
  const draftSession = useRef(0);
  const configurationsRef = useRef(configurations);
  configurationsRef.current = configurations;
  const reorderSeqRef = useRef(0);
  const [placement, setPlacement] = useState<{ left: number; top: number; maxHeight: number } | null>(null);
  const [unavailableById, setUnavailableById] = useState(() => new Map(configurationRows.map((row) => [row.id, row.unavailable_reason])));
  const mounted = useRef(false);
  const sensors = useSensors(
    useSensor(PointerSensor, { activationConstraint: { distance: 4 } }),
    useSensor(KeyboardSensor, { coordinateGetter: sortableKeyboardCoordinates }),
  );


  useEffect(() => {
    onEditingChange(draft !== null || dragging);
    return () => onEditingChange(false);
  }, [draft, dragging, onEditingChange]);

  useEffect(() => {
    mounted.current = true;
    return () => { mounted.current = false; };
  }, []);

  useEffect(() => {
    setUnavailableById(new Map(configurationRows.map((row) => [row.id, row.unavailable_reason])));
  }, [configurationRows]);

  useEffect(() => {
    let current = true;
    listSpawnConfigurations().then((values) => {
      if (!current) return;
      setConfigurations(values.filter((value) => belongsToHarness(value, option.harness_id) && offeredUnderLogin(value, login?.id)));
      setLoaded(true);
    }).catch((e: unknown) => { if (current) setError(String(e)); });
    return () => { current = false; };
  }, [option.id, option.harness_id, option.configuration, login?.id]);

  const editing = draft !== null;
  useEffect(() => {
    if (!editing) return;
    let current = true;
    setTargetError(null);
    getLaunchTargets().then((values) => { if (current) { setTargets(values); setTargetError(null); } })
      .catch((e: unknown) => { if (current) setTargetError(String(e)); });
    return () => { current = false; };
  }, [editing, draft?.id]);

  useLayoutEffect(() => {
    // The top layer escapes animated/scrolling ancestors while the DOM stays
    // inside the canvas dialog's focus trap.
    panel.current?.showPopover?.();
    const place = () => {
      const rect = anchor.getBoundingClientRect();
      // A detached or hidden anchor has no layout. Keep the last placement
      // instead of measuring a zero rect, which used to snap the panel to the
      // top-left corner of the screen, far from the row it belongs to.
      if (rect.width === 0 && rect.height === 0) return;
      // `scrollHeight` is the panel's natural content height; `offsetHeight`
      // would report the height already capped by the previous placement.
      const { left, top, maxHeight } = placeFlyout(rect, panel.current?.offsetWidth ?? 280,
        panel.current?.scrollHeight ?? 0, window.innerWidth, window.innerHeight);
      setPlacement((current) => current
        && current.left === left
        && current.top === top
        && current.maxHeight === maxHeight
        ? current
        : { left, top, maxHeight });
    };
    place();
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    // The editor owns its own draft state: switching to a routed provider adds
    // endpoint fields without changing this component's props, so the panel's
    // content is observed for size changes and the placement follows it.
    let frame: number | null = null;
    const schedulePlace = () => {
      if (frame !== null) return;
      frame = window.requestAnimationFrame(() => { frame = null; place(); });
    };
    const Observer = typeof ResizeObserver === 'undefined' ? null : ResizeObserver;
    const observer = Observer ? new Observer(schedulePlace) : null;
    if (panel.current) {
      observer?.observe(panel.current);
      for (const child of Array.from(panel.current.children)) observer?.observe(child);
    }
    return () => {
      window.removeEventListener('resize', place);
      window.removeEventListener('scroll', place, true);
      if (frame !== null) window.cancelAnimationFrame(frame);
      observer?.disconnect();
    };
  }, [anchor, draft, configurations, error, targetError, targets, loaded]);

  useEffect(() => {
    if (keyboard) panel.current?.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
  }, [keyboard, draft]);

  const wasEditing = useRef(false);
  useEffect(() => {
    if (wasEditing.current && !draft) {
      setActiveMenuIndex(0);
      panel.current?.querySelector<HTMLElement>('[role="menuitem"]')?.focus();
    }
    wasEditing.current = draft !== null;
  }, [draft]);

  const close = () => { onClose(); anchor.focus({ preventScroll: true }); };
  const openDraft = (value: SpawnConfiguration) => {
    draftSession.current += 1;
    setDraft(value);
  };
  const cancelDraft = () => {
    draftSession.current += 1;
    setDraft(null);
    setError(null);
  };
  const replaceConfiguration = (saved: SpawnConfiguration, harnessId?: string) => {
    setConfigurations((previous) => {
      const index = previous.findIndex((entry) => entry.id === saved.id);
      if (harnessId !== option.harness_id || !offeredUnderLogin(saved, login?.id)) return previous.filter((entry) => entry.id !== saved.id);
      if (index < 0) return [...previous, saved];
      const next = [...previous];
      next[index] = saved;
      return next;
    });
  };
  const refreshAvailability = async (saved: SpawnConfiguration, session: number) => {
    try {
      const savedRow = (await listProviders()).find((row) => row.id === saved.id);
      if (!mounted.current) return;
      replaceConfiguration(saved, savedRow?.harness_id);
      setUnavailableById((previous) => {
        const next = new Map(previous);
        if (savedRow) next.set(saved.id, (decorate?.(savedRow) ?? savedRow).unavailable_reason);
        else next.delete(saved.id);
        return next;
      });
    } catch (e) {
      if (mounted.current && draftSession.current === session) setError(`Saved, but recipe availability could not be refreshed: ${String(e)}`);
    }
  };
  const persistConfiguration = async (value: SpawnConfiguration, route?: ProviderPairing, onRefreshing?: () => void) => {
    // Issue #1948: emit one `launch_config_timing:` checkpoint per save
    // phase so a reader grepping `launch_config_timing:` across
    // `buildmesh.log` (via the frontendLog `console.info` bridge) and the
    // browser console gets the caller-side split — persistence IPC versus
    // menu refresh — next to the backend's own phase timings. Elapsed only:
    // never configuration names, ids, URLs, models, or credentials.
    const session = draftSession.current;
    const saveStarted = performance.now();
    const saved = route ? await saveSpawnConfiguration(value, route) : await saveSpawnConfiguration(value);
    console.info(`launch_config_timing: checkpoint=save elapsed=${Math.round(performance.now() - saveStarted)}ms`);
    if (!mounted.current) return;
    replaceConfiguration(saved, saved.harness_id ?? saved.spawn_option_id.split(':')[0]);
    onRefreshing?.();
    const refreshStarted = performance.now();
    await refreshAvailability(saved, session);
    console.info(`launch_config_timing: checkpoint=refresh elapsed=${Math.round(performance.now() - refreshStarted)}ms`);
    if (mounted.current && draftSession.current === session) setDraft(null);
  };
  // Every launch from a login's submenu carries that login.
  const select = login
    ? (providerId: string, altKey: boolean, configurationId?: string) => onSelect(providerId, altKey, configurationId, login.id)
    : onSelect;
  const canReorder = configurations.length >= 2;
  const handleDragEnd = (event: DragEndEvent) => {
    setDragging(false);
    const { active, over } = event;
    if (!over || active.id === over.id) return;
    const previous = configurationsRef.current;
    const currentIds = previous.map((entry) => entry.id);
    const nextIds = reorderConfigurationIds(currentIds, String(active.id), String(over.id));
    if (nextIds === currentIds) return;
    const byId = new Map(previous.map((entry) => [entry.id, entry]));
    const next = nextIds
      .map((id) => byId.get(id))
      .filter((entry): entry is SpawnConfiguration => Boolean(entry));
    const seq = ++reorderSeqRef.current;
    configurationsRef.current = next;
    setConfigurations(next);
    setError(null);
    void reorderSpawnConfigurations(option.harness_id, nextIds).catch((e: unknown) => {
      if (!mounted.current) return;
      if (reorderSeqRef.current === seq) {
        configurationsRef.current = previous;
        setConfigurations(previous);
      }
      setError(String(e));
    });
  };
  const menuClass = 'w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-selection focus:bg-bg-selection focus:outline-none';
  const mutedMenuClass = 'w-full px-3 py-1.5 text-left text-xs text-text-muted';
  const rowActionClass = 'px-2 text-xs text-text-secondary hover:bg-bg-selection focus:bg-bg-selection';
  const menuEntries = [
    { id: 'defaults', kind: 'spawn' as const },
    ...configurations.flatMap((value) => [
      { id: `configuration:${value.id}`, kind: 'configuration' as const, value },
      { id: `edit:${value.id}`, kind: 'edit' as const, value },
      { id: `clone:${value.id}`, kind: 'clone' as const, value },
      ...(canReorder ? [{ id: `reorder:${value.id}`, kind: 'reorder' as const, value }] : []),
    ]),
    { id: 'new', kind: 'new' as const },
  ];
  const menuIndex = new Map(menuEntries.map((entry, index) => [entry.id, index]));
  const defaultsUnavailable = (decorate?.(option) ?? option).unavailable_reason;
  return createPortal(
    <div
      ref={panel}
      popover={typeof HTMLElement.prototype.showPopover === 'function' ? 'manual' : undefined}
      data-dropdown-for={anchor.closest('[data-dropdown-for]')?.getAttribute('data-dropdown-for') ?? undefined}
      data-testid="spawn-configurations"
      style={{
        position: 'fixed', margin: 0, right: 'auto', bottom: 'auto',
        left: placement?.left ?? 0,
        top: placement?.top ?? 0,
        width: 'min(280px, calc(100vw - 16px))',
        maxHeight: placement ? `${placement.maxHeight}px` : 'calc(100vh - 16px)',
        zIndex: 1000,
      }}
      className="overflow-y-auto rounded-md border border-border-default bg-bg-overlay shadow-md"
      onClick={(e) => e.stopPropagation()}
      onKeyDown={(e) => {
        if (dragging) return;
        if (e.key === 'Tab') {
          if (draft) {
            const fields = Array.from(panel.current?.querySelectorAll<HTMLElement>('input:not(:disabled), select:not(:disabled), button:not(:disabled)') ?? []);
            const first = fields[0];
            const last = fields[fields.length - 1];
            if (e.shiftKey && document.activeElement === first) {
              e.preventDefault();
              e.stopPropagation();
              last?.focus();
            } else if (!e.shiftKey && document.activeElement === last) {
              e.preventDefault();
              e.stopPropagation();
              first?.focus();
            }
          } else {
            e.preventDefault();
            e.stopPropagation();
            onDismiss();
          }
          return;
        }
        e.stopPropagation();
        if (e.key === 'Escape' || (!draft && e.key === 'ArrowLeft')) {
          e.preventDefault();
          if (draft && !option.configuration) cancelDraft(); else close();
        }
        if (draft) return;
        const items = Array.from(panel.current?.querySelectorAll<HTMLElement>('[role="menuitem"]:not(:disabled)') ?? []);
        const index = items.indexOf(document.activeElement as HTMLElement);
        const next = e.key === 'ArrowDown' ? (index + 1) % items.length
          : e.key === 'ArrowUp' ? (index - 1 + items.length) % items.length
          : e.key === 'Home' ? 0 : e.key === 'End' ? items.length - 1 : null;
        if (next !== null) {
          e.preventDefault();
          setActiveMenuIndex(next);
          items[next]?.focus();
        }
      }}
    >
      {draft ? <LaunchConfigurationEditor key={draftSession.current} value={draft} targets={targets} onCancel={option.configuration ? () => { cancelDraft(); close(); } : cancelDraft}
        onSave={persistConfiguration}
        onVerify={verifyLaunchConfiguration}
        onDelete={draft.id ? async () => {
          const session = draftSession.current;
          const deletedId = draft.id;
          await deleteSpawnConfiguration(deletedId);
          if (!mounted.current) return;
          setConfigurations((previous) => previous.filter((entry) => entry.id !== deletedId));
          setUnavailableById((previous) => {
            const next = new Map(previous);
            next.delete(deletedId);
            return next;
          });
          if (draftSession.current === session) cancelDraft();
        } : undefined} /> : (
        <div role="menu" aria-label={login ? `${option.label} ${login.label} configurations` : `${option.label} configurations`}>
          <button type="button" role="menuitem" data-menu-index={menuIndex.get('defaults')} tabIndex={activeMenuIndex === menuIndex.get('defaults') ? 0 : -1} className={menuClass}
            aria-disabled={Boolean(defaultsUnavailable)} title={defaultsUnavailable}
            onClick={(e) => { if (!defaultsUnavailable) select(option.id, e.altKey); }}>Spawn with defaults</button>
          {!loaded && !error && <p role="presentation" className="px-3 py-1.5 text-xs text-text-muted">Loading configurations…</p>}
          {loaded && configurations.length === 0 && <p role="presentation" className="px-3 py-1.5 text-xs text-text-muted">No saved configurations</p>}
          <DndContext
            sensors={sensors}
            onDragStart={() => setDragging(true)}
            onDragCancel={() => setDragging(false)}
            onDragEnd={handleDragEnd}
          >
            <SortableContext items={configurations.map((c) => c.id)} strategy={verticalListSortingStrategy}>
              {configurations.map((value) => {
                const unavailable = unavailableById.get(value.id) ?? decorate?.({ ...option, id: value.id, configuration: value }).unavailable_reason;
                return (
                  <SortableMenuConfigurationRow
                    key={value.id}
                    value={value}
                    unavailable={unavailable}
                    canReorder={canReorder}
                    activeMenuIndex={activeMenuIndex}
                    configurationIndex={menuIndex.get(`configuration:${value.id}`)}
                    editIndex={menuIndex.get(`edit:${value.id}`)}
                    cloneIndex={menuIndex.get(`clone:${value.id}`)}
                    reorderIndex={menuIndex.get(`reorder:${value.id}`)}
                    menuClass={menuClass}
                    rowActionClass={rowActionClass}
                    onSelect={(altKey) => select(value.spawn_option_id, altKey, value.id)}
                    onEdit={() => openDraft(value)}
                    onClone={() => openDraft(cloneConfiguration(value))}
                  />
                );
              })}
            </SortableContext>
          </DndContext>
          <button type="button" role="menuitem" data-menu-index={menuIndex.get('new')} tabIndex={activeMenuIndex === menuIndex.get('new') && loaded ? 0 : -1} disabled={!loaded} className={loaded ? menuClass : mutedMenuClass} onClick={() => openDraft({ id: '', name: '', spawn_option_id: option.id, model: null, effort: null, extra_args: null })}>New configuration…</button>
        </div>
      )}
      {(error || (draft && targetError)) && <p role="alert" className="p-3 text-xs text-status-error">{error ?? targetError}</p>}
    </div>,
    anchor.closest('[role="dialog"]') ?? document.body,
  );
}
