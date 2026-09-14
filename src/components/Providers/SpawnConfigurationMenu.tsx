import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import type { SpawnOption } from '../../lib/groups';
import type { SpawnConfiguration } from '../../types/generated/SpawnConfiguration';
import { deleteSpawnConfiguration, listSpawnConfigurations, saveSpawnConfiguration } from '../../lib/tauri/provider';

export function SpawnConfigurationMenu({ option, anchor, keyboard, onSelect, onClose, onDismiss, onEditingChange }: {
  option: SpawnOption;
  anchor: HTMLElement;
  keyboard: boolean;
  onSelect: (providerId: string, altKey: boolean, configurationId?: string) => void;
  onClose: () => void;
  onDismiss: () => void;
  onEditingChange: (editing: boolean) => void;
}) {
  const [configurations, setConfigurations] = useState<SpawnConfiguration[]>([]);
  const [loaded, setLoaded] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [draft, setDraft] = useState<SpawnConfiguration | null>(null);
  const [activeMenuIndex, setActiveMenuIndex] = useState(0);
  const [busy, setBusy] = useState(false);
  const panel = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState({ left: 0, top: 0 });
  const caps = option.capabilities;
  const effort = caps?.effort_control;
  const allowed = effort && effort.kind !== 'none' ? effort.allowed : [];

  useEffect(() => {
    onEditingChange(draft !== null);
    return () => onEditingChange(false);
  }, [draft, onEditingChange]);

  useEffect(() => {
    let current = true;
    listSpawnConfigurations().then((values) => {
      if (!current) return;
      setConfigurations(values.filter((v) => v.spawn_option_id === option.id));
      setLoaded(true);
    }).catch((e: unknown) => { if (current) setError(String(e)); });
    return () => { current = false; };
  }, [option.id]);

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
  }, [anchor, draft, configurations, error, loaded]);

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
  const save = async () => {
    if (!draft || busy) return;
    setBusy(true);
    setError(null);
    try {
      const saved = await saveSpawnConfiguration(draft);
      setConfigurations((prev) => {
        const index = prev.findIndex((v) => v.id === saved.id);
        if (index < 0) return [...prev, saved];
        const next = [...prev];
        next[index] = saved;
        return next;
      });
      setDraft(null);
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  const remove = async () => {
    if (!draft?.id || busy) return;
    setBusy(true);
    setError(null);
    try {
      await deleteSpawnConfiguration(draft.id);
      setConfigurations((prev) => prev.filter((v) => v.id !== draft.id));
      setDraft(null);
    } catch (e) { setError(String(e)); }
    finally { setBusy(false); }
  };
  const fieldClass = 'w-full border border-border-subtle rounded-md bg-bg-card px-2 py-1 text-sm text-text-primary';
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
          if (!busy) { if (draft) setDraft(null); else close(); }
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
      {draft ? (
        <form className="space-y-3 p-3" aria-label="Edit spawn configuration" onSubmit={(e) => { e.preventDefault(); void save(); }}>
          <p className="text-sm font-medium text-text-primary">{draft.id ? 'Edit configuration' : 'New configuration'}</p>
          <p className="text-xs text-text-muted">{option.label}. Unset fields inherit current defaults.</p>
          <fieldset disabled={busy} className="space-y-3">
            <label className="block text-xs text-text-secondary">Name
              <input autoFocus required value={draft.name} onChange={(e) => setDraft({ ...draft, name: e.target.value })} className={fieldClass} />
            </label>
            {caps?.supports_model_override && <label className="block text-xs text-text-secondary">Model
              <input value={draft.model ?? ''} placeholder="Default" onChange={(e) => setDraft({ ...draft, model: e.target.value || null })} className={fieldClass} />
            </label>}
            {allowed.length > 0 && <label className="block text-xs text-text-secondary">Effort
              <select aria-label="Effort" value={draft.effort ?? ''} onChange={(e) => setDraft({ ...draft, effort: e.target.value || null })} className={fieldClass}>
                <option value="">Default</option>
                {allowed.map((value) => <option key={value} value={value}>{value}</option>)}
              </select>
            </label>}
            {caps?.supports_extra_args && <label className="block text-xs text-text-secondary">Extra arguments
              <input value={draft.extra_args ?? ''} onChange={(e) => setDraft({ ...draft, extra_args: e.target.value || null })} className={fieldClass} />
            </label>}
            <div className="flex gap-3 text-sm text-text-primary">
              <button type="submit" disabled={!draft.name.trim()}>Save</button>
              <button type="button" onClick={() => setDraft(null)}>Cancel</button>
              {draft.id && <button type="button" onClick={() => void remove()}>Delete</button>}
            </div>
          </fieldset>
        </form>
      ) : (
        <div role="menu" aria-label={`${option.label} configurations`}>
          <button type="button" role="menuitem" data-menu-index={menuIndex.get('defaults')} tabIndex={activeMenuIndex === menuIndex.get('defaults') ? 0 : -1} className={menuClass} onClick={(e) => onSelect(option.id, e.altKey)}>Spawn with defaults</button>
          {!loaded && !error && <p role="presentation" className="px-3 py-2 text-xs text-text-muted">Loading configurations…</p>}
          {loaded && configurations.length === 0 && <p role="presentation" className="px-3 py-2 text-xs text-text-muted">No saved configurations</p>}
          {configurations.map((value) => <div key={value.id} role="presentation" className="flex">
            <button type="button" role="menuitem" data-menu-index={menuIndex.get(`configuration:${value.id}`)} tabIndex={activeMenuIndex === menuIndex.get(`configuration:${value.id}`) ? 0 : -1} className={`${menuClass} min-w-0 flex-1 break-words`} onClick={(e) => onSelect(option.id, e.altKey, value.id)}>{value.name}</button>
            <button type="button" role="menuitem" data-menu-index={menuIndex.get(`edit:${value.id}`)} tabIndex={activeMenuIndex === menuIndex.get(`edit:${value.id}`) ? 0 : -1} aria-label={`Edit ${value.name}`} className="px-3 text-xs text-text-secondary hover:bg-bg-selection focus:bg-bg-selection" onClick={() => setDraft(value)}>Edit</button>
          </div>)}
          <button type="button" role="menuitem" data-menu-index={menuIndex.get('new')} tabIndex={activeMenuIndex === menuIndex.get('new') && loaded ? 0 : -1} disabled={!loaded} className={menuClass} onClick={() => setDraft({ id: '', name: '', spawn_option_id: option.id, model: null, effort: null, extra_args: null })}>New configuration…</button>
        </div>
      )}
      {error && <p role="alert" className="p-3 text-xs text-status-error">{error}</p>}
    </div>,
    anchor.closest('[role="dialog"]') ?? document.body,
  );
}
