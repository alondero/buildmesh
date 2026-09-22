import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import type { SpawnOption } from '../../lib/groups';
import type { SpawnConfiguration } from '../../types/generated/SpawnConfiguration';
import { deleteSpawnConfiguration, getLaunchTargets, listSpawnConfigurations, saveSpawnConfiguration } from '../../lib/tauri/provider';
import { LaunchConfigurationEditor } from './LaunchConfigurationEditor';
import type { LaunchTarget } from '../../types/generated/LaunchTarget';

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
  const [position, setPosition] = useState({ left: 0, top: 0 });
  const unavailableById = new Map(configurationRows.map((row) => [row.id, row.unavailable_reason]));

  useEffect(() => {
    onEditingChange(draft !== null);
    return () => onEditingChange(false);
  }, [draft, onEditingChange]);

  useEffect(() => {
    let current = true;
    listSpawnConfigurations().then((values) => {
      if (!current) return;
      setConfigurations(values.filter((v) => (v.harness_id ?? v.spawn_option_id.split(':')[0]) === option.harness_id));
      setLoaded(true);
    }).catch((e: unknown) => { if (current) setError(String(e)); });
    return () => { current = false; };
  }, [option.id, option.harness_id, option.configuration]);

  useEffect(() => {
    if (!draft) return;
    let current = true;
    getLaunchTargets().then((values) => { if (current) { setTargets(values); setTargetError(null); } })
      .catch((e: unknown) => { if (current) setTargetError(String(e)); });
    return () => { current = false; };
  }, [draft]);

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
  const menuClass = 'w-full px-3 py-2 text-left text-sm text-text-primary hover:bg-bg-selection focus:bg-bg-selection focus:outline-none';
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
          if (draft && !option.configuration) setDraft(null); else close();
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
      {draft ? <LaunchConfigurationEditor value={draft} targets={targets} onCancel={option.configuration ? close : () => setDraft(null)}
        onSave={async (value) => { await saveSpawnConfiguration(value); onDismiss(); }}
        onDelete={draft.id ? async () => { await deleteSpawnConfiguration(draft.id); onDismiss(); } : undefined} /> : (
        <div role="menu" aria-label={`${option.label} configurations`}>
          <button type="button" role="menuitem" data-menu-index={menuIndex.get('defaults')} tabIndex={activeMenuIndex === menuIndex.get('defaults') ? 0 : -1} className={menuClass} onClick={(e) => onSelect(option.id, e.altKey)}>Spawn with defaults</button>
          {!loaded && !error && <p role="presentation" className="px-3 py-2 text-xs text-text-muted">Loading configurations…</p>}
          {loaded && configurations.length === 0 && <p role="presentation" className="px-3 py-2 text-xs text-text-muted">No saved configurations</p>}
          {configurations.map((value) => <div key={value.id} role="presentation" className="flex">
            <button type="button" role="menuitem" data-menu-index={menuIndex.get(`configuration:${value.id}`)} tabIndex={activeMenuIndex === menuIndex.get(`configuration:${value.id}`) ? 0 : -1}
              aria-disabled={Boolean(unavailableById.get(value.id))} title={unavailableById.get(value.id) ?? undefined}
              className={`w-full min-w-0 flex-1 break-words px-3 py-2 text-left text-sm focus:outline-none ${unavailableById.get(value.id) ? 'cursor-default text-text-muted' : 'text-text-primary hover:bg-bg-selection focus:bg-bg-selection'}`} onClick={(e) => { if (!unavailableById.get(value.id)) onSelect(value.spawn_option_id, e.altKey, value.id); }}>
              {value.name}{unavailableById.get(value.id) && <span className="block text-text-muted">{unavailableById.get(value.id)}</span>}
            </button>
            <button type="button" role="menuitem" data-menu-index={menuIndex.get(`edit:${value.id}`)} tabIndex={activeMenuIndex === menuIndex.get(`edit:${value.id}`) ? 0 : -1} aria-label={`Edit ${value.name}`} className="px-3 text-xs text-text-secondary hover:bg-bg-selection focus:bg-bg-selection" onClick={() => setDraft(value)}>Edit</button>
          </div>)}
          <button type="button" role="menuitem" data-menu-index={menuIndex.get('new')} tabIndex={activeMenuIndex === menuIndex.get('new') && loaded ? 0 : -1} disabled={!loaded} className={loaded ? menuClass : 'w-full px-3 py-2 text-left text-sm text-text-muted'} onClick={() => setDraft({ id: '', name: '', spawn_option_id: option.id, model: null, effort: null, extra_args: null })}>New configuration…</button>
        </div>
      )}
      {(error || (draft && targetError)) && <p role="alert" className="p-3 text-xs text-status-error">{error ?? targetError}</p>}
    </div>,
    anchor.closest('[role="dialog"]') ?? document.body,
  );
}
