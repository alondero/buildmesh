import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import type { SpawnOption } from '../../lib/groups';
import type { SpawnConfiguration } from '../../types/generated/SpawnConfiguration';
import { deleteSpawnConfiguration, getLaunchTargets, listProviders, listSpawnConfigurations, saveSpawnConfiguration, verifyLaunchConfiguration } from '../../lib/tauri/provider';
import { LaunchConfigurationEditor } from './LaunchConfigurationEditor';
import type { ProviderPairing } from '../../types/generated/ProviderPairing';
import type { LaunchTarget } from '../../types/generated/LaunchTarget';

function belongsToHarness(configuration: SpawnConfiguration, harnessId: string): boolean {
  return (configuration.harness_id ?? configuration.spawn_option_id.split(':')[0]) === harnessId;
}

export function SpawnConfigurationMenu({ option, anchor, keyboard, configurationRows, onSelect, onClose, onDismiss, onEditingChange }: {
  option: SpawnOption;
  anchor: HTMLElement;
  keyboard: boolean;
  configurationRows: SpawnOption[];
  onSelect: (providerId: string, altKey: boolean, configurationId?: string) => void;
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
  const panel = useRef<HTMLDivElement>(null);
  const draftSession = useRef(0);
  const [position, setPosition] = useState({ left: 0, top: 0 });
  const [unavailableById, setUnavailableById] = useState(() => new Map(configurationRows.map((row) => [row.id, row.unavailable_reason])));
  const mounted = useRef(false);


  useEffect(() => {
    onEditingChange(draft !== null);
    return () => onEditingChange(false);
  }, [draft, onEditingChange]);

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
      setConfigurations(values.filter((value) => belongsToHarness(value, option.harness_id)));
      setLoaded(true);
    }).catch((e: unknown) => { if (current) setError(String(e)); });
    return () => { current = false; };
  }, [option.id, option.harness_id, option.configuration]);

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
      const width = panel.current?.offsetWidth ?? 280;
      const height = panel.current?.offsetHeight ?? 300;
      const left = rect.right + width <= window.innerWidth - 8 ? rect.right : rect.left - width;
      setPosition({
        left: Math.max(8, Math.min(left, window.innerWidth - width - 8)),
        top: Math.max(8, Math.min(rect.top, window.innerHeight - height - 8)),
      });
    };
    place();
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    return () => {
      window.removeEventListener('resize', place);
      window.removeEventListener('scroll', place, true);
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
      if (harnessId !== option.harness_id) return previous.filter((entry) => entry.id !== saved.id);
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
        if (savedRow) next.set(saved.id, savedRow.unavailable_reason);
        else next.delete(saved.id);
        return next;
      });
    } catch (e) {
      if (mounted.current && draftSession.current === session) setError(`Saved, but recipe availability could not be refreshed: ${String(e)}`);
    }
  };
  const persistConfiguration = async (value: SpawnConfiguration, route?: ProviderPairing) => {
    const session = draftSession.current;
    const saved = route ? await saveSpawnConfiguration(value, route) : await saveSpawnConfiguration(value);
    if (!mounted.current) return;
    replaceConfiguration(saved, saved.harness_id ?? saved.spawn_option_id.split(':')[0]);
    await refreshAvailability(saved, session);
    if (mounted.current && draftSession.current === session) setDraft(null);
  };
  const menuClass = 'w-full px-3 py-2 text-left text-sm text-text-primary hover:bg-bg-selection focus:bg-bg-selection focus:outline-none';
  const mutedMenuClass = 'w-full px-3 py-2 text-left text-sm text-text-muted';
  const menuEntries = [
    { id: 'defaults', kind: 'spawn' as const },
    ...configurations.flatMap((value) => [
      { id: `configuration:${value.id}`, kind: 'configuration' as const, value },
      { id: `edit:${value.id}`, kind: 'edit' as const, value },
    ]),
    { id: 'new', kind: 'new' as const },
  ];
  const menuIndex = new Map(menuEntries.map((entry, index) => [entry.id, index]));
  return createPortal(
    <div
      ref={panel}
      popover={typeof HTMLElement.prototype.showPopover === 'function' ? 'manual' : undefined}
      data-dropdown-for={anchor.closest('[data-dropdown-for]')?.getAttribute('data-dropdown-for') ?? undefined}
      data-testid="spawn-configurations"
      style={{ position: 'fixed', margin: 0, right: 'auto', bottom: 'auto', ...position, width: 'min(280px, calc(100vw - 16px))', maxHeight: 'calc(100vh - 16px)', zIndex: 1000 }}
      className="overflow-y-auto rounded-md border border-border-default bg-bg-overlay shadow-md"
      onClick={(e) => e.stopPropagation()}
      onKeyDown={(e) => {
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
        <div role="menu" aria-label={`${option.label} configurations`}>
          <button type="button" role="menuitem" data-menu-index={menuIndex.get('defaults')} tabIndex={activeMenuIndex === menuIndex.get('defaults') ? 0 : -1} className={menuClass} onClick={(e) => onSelect(option.id, e.altKey)}>Spawn with defaults</button>
          {!loaded && !error && <p role="presentation" className="px-3 py-2 text-xs text-text-muted">Loading configurations…</p>}
          {loaded && configurations.length === 0 && <p role="presentation" className="px-3 py-2 text-xs text-text-muted">No saved configurations</p>}
          {configurations.map((value) => {
            const unavailable = unavailableById.get(value.id);
            return <div key={value.id} role="presentation" className="flex">
              <button type="button" role="menuitem" data-menu-index={menuIndex.get(`configuration:${value.id}`)} tabIndex={activeMenuIndex === menuIndex.get(`configuration:${value.id}`) ? 0 : -1}
                aria-disabled={Boolean(unavailable)} title={unavailable ?? undefined}
                className={`${menuClass} min-w-0 flex-1 break-words ${unavailable ? 'cursor-default text-text-muted' : ''}`} onClick={(e) => { if (!unavailable) onSelect(value.spawn_option_id, e.altKey, value.id); }}>
                {value.name}{unavailable && <span className="block text-text-muted">{unavailable}</span>}
              </button>
              <button type="button" role="menuitem" data-menu-index={menuIndex.get(`edit:${value.id}`)} tabIndex={activeMenuIndex === menuIndex.get(`edit:${value.id}`) ? 0 : -1} aria-label={`Edit ${value.name}`} className="px-3 text-xs text-text-secondary hover:bg-bg-selection focus:bg-bg-selection" onClick={() => openDraft(value)}>Edit</button>
            </div>;
          })}
          <button type="button" role="menuitem" data-menu-index={menuIndex.get('new')} tabIndex={activeMenuIndex === menuIndex.get('new') && loaded ? 0 : -1} disabled={!loaded} className={loaded ? menuClass : mutedMenuClass} onClick={() => openDraft({ id: '', name: '', spawn_option_id: option.id, model: null, effort: null, extra_args: null })}>New configuration…</button>
        </div>
      )}
      {(error || (draft && targetError)) && <p role="alert" className="p-3 text-xs text-status-error">{error ?? targetError}</p>}
    </div>,
    anchor.closest('[role="dialog"]') ?? document.body,
  );
}
