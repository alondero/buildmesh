/**
 * SpawnOptionPicker — the Spawn Menu reused as a *settings* control.
 *
 * The provider `<select>`s in Project Settings and App Settings used to be
 * flat lists built from the raw provider rows, so their options diverged
 * from every spawn surface (bare Proxied Provider routes shown flat,
 * Launch Configurations only incidentally present). This picker renders the
 * exact same `GroupedProviderMenu` the `+ ▾` cluster uses — native harness
 * parents with a `›` submenu of the user's saved Launch Configurations
 * (including "New configuration…" / per-row "Edit").
 *
 * Selecting a configuration stores its id; the backend resolver
 * (`launch_configurations::resolve`) already treats a configuration id as a
 * valid selection, so `meshes.default_provider` / `preferences.default_provider`
 * / `reviewer_provider` / `naming_provider` can point at a recipe.
 *
 * The panel is rendered in the top layer (a portal + the native `popover`
 * API when available) so it is never clipped by the Probe panel's or the
 * Settings modal's scroll containers — mirroring `SpawnConfigurationMenu`.
 */
import { useEffect, useId, useLayoutEffect, useMemo, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import type { ProviderInfo } from '../../types/generated/ProviderInfo';
import { mapBackendProviders, type SpawnOption } from '../../lib/groups';
import { listSpawnConfigurations } from '../../lib/tauri/provider';
import { GroupedProviderMenu } from './GroupedProviderMenu';

/**
 * Resolve a Spawn Option / Launch Configuration id to its human label.
 *
 * The provider list already carries saved Launch Configurations as rows
 * (`configuration_menu` appends them), so most ids resolve from `providers`.
 * A configuration that is *not* in the menu — its harness is hidden on this
 * host, or the id was stored before the recipe became unavailable — would
 * otherwise fall back to the raw `launch/<uuid>` id on the trigger, so this
 * falls back to `listSpawnConfigurations()` for `launch/`-prefixed ids.
 * Returns `null` when the id cannot be resolved (caller decides the fallback).
 */
export function useSpawnOptionLabel(providers: ProviderInfo[], id: string | null): string | null {
  const options = useMemo(() => mapBackendProviders(providers), [providers]);
  const fromOptions = id ? options.find((option) => option.id === id)?.label : undefined;
  const [fetchedName, setFetchedName] = useState<string | null>(null);

  useEffect(() => {
    if (!id || fromOptions || !id.startsWith('launch/')) {
      setFetchedName(null);
      return;
    }
    let current = true;
    listSpawnConfigurations()
      .then((configurations) => {
        if (current) setFetchedName(configurations.find((value) => value.id === id)?.name ?? null);
      })
      .catch(() => {
        if (current) setFetchedName(null);
      });
    return () => {
      current = false;
    };
  }, [id, fromOptions]);

  if (!id) return null;
  return fromOptions ?? fetchedName ?? null;
}

export interface SpawnOptionPickerProps {
  /** Optional id for the trigger button (label `htmlFor` targets). */
  id?: string;
  ariaLabel: string;
  /** Raw backend rows (`listProviders()` / `useProviderList`) — mapped
   *  internally to the frontend `SpawnOption` shape. */
  providers: ProviderInfo[];
  /** Current selection id, or `null` for "inherit / unset". */
  value: string | null;
  /** Label shown when `value` is `null`. */
  unsetLabel: string;
  /** Value persisted when the user picks the unset row. */
  unsetValue: string | null;
  onSelect: (value: string | null) => void;
  /** Optional per-row filter (e.g. exclude Terminal for the reviewer picker). */
  filter?: (option: SpawnOption) => boolean;
  /** Optional per-row decoration applied after the backend→frontend
   *  projection — e.g. stamp `unavailable_reason` on a harness the caller
   *  knows can't serve the role so the row renders disabled with a reason. */
  decorate?: (option: SpawnOption) => SpawnOption;
  disabled?: boolean;
  /** Visual density: `sm` for the Probe panel, `md` for the Settings modal. */
  size?: 'sm' | 'md';
  className?: string;
}

export function SpawnOptionPicker({
  id,
  ariaLabel,
  providers,
  value,
  unsetLabel,
  unsetValue,
  onSelect,
  filter,
  decorate,
  disabled,
  size = 'sm',
  className,
}: SpawnOptionPickerProps) {
  const [open, setOpen] = useState(false);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const panelRef = useRef<HTMLDivElement>(null);
  const [position, setPosition] = useState({ left: 0, top: 0, width: 240 });
  // Stable key mirrored onto the panel's `data-dropdown-for`. The nested
  // `SpawnConfigurationMenu` (and its editor) portal outside this panel and
  // inherit the same key, so the outside-click guard below can recognise
  // them as part of this picker.
  const menuKey = useId();

  const options = useMemo(() => {
    const mapped = mapBackendProviders(providers);
    return decorate ? mapped.map(decorate) : mapped;
  }, [providers, decorate]);
  const resolvedLabel = useSpawnOptionLabel(providers, value);
  const label = value ? resolvedLabel ?? value : unsetLabel;

  useEffect(() => {
    if (!open) return;
    const handler = (event: MouseEvent) => {
      const target = event.target as Node | null;
      if (!target) return;
      if (panelRef.current?.contains(target) || triggerRef.current?.contains(target)) return;
      // The configuration submenu (and its editor) portal to the top layer,
      // outside `panelRef`; they carry our `data-dropdown-for` key so a click
      // there is not swallowed as an outside click (which would unmount the
      // menu before the item's `click` fires).
      if (target instanceof Element && target.closest(`[data-dropdown-for="${menuKey}"]`)) return;
      setOpen(false);
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [open, menuKey]);

  useLayoutEffect(() => {
    if (!open) return;
    // Escapes animated/scrolling ancestors while the DOM stays inside the
    // surrounding dialog's focus trap (same trick `SpawnConfigurationMenu` uses).
    panelRef.current?.showPopover?.();
    const place = () => {
      const rect = triggerRef.current?.getBoundingClientRect();
      if (!rect) return;
      setPosition({ left: rect.left, top: rect.bottom + 4, width: Math.max(rect.width, 240) });
    };
    place();
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    return () => {
      window.removeEventListener('resize', place);
      window.removeEventListener('scroll', place, true);
    };
  }, [open]);

  const close = () => {
    setOpen(false);
    triggerRef.current?.focus({ preventScroll: true });
  };

  const pick = (next: string | null) => {
    onSelect(next);
    close();
  };

  const triggerClass =
    size === 'md'
      ? 'w-full flex items-center justify-between gap-2 bg-bg-card border border-border-subtle rounded-md px-4 py-2.5 text-base text-text-primary focus:outline-none focus:border-accent-cyan disabled:opacity-50'
      : 'w-full flex items-center justify-between gap-2 bg-bg-overlay border border-border-subtle rounded-md px-2 py-1.5 text-sm text-text-primary focus:outline-none focus:border-accent-cyan disabled:opacity-50';
  const unsetClass =
    size === 'md'
      ? 'w-full px-4 py-2.5 text-left text-base text-text-primary hover:bg-bg-selection focus:bg-bg-selection focus:outline-none'
      : 'w-full px-3 py-1.5 text-left text-xs text-text-primary hover:bg-bg-selection focus:bg-bg-selection focus:outline-none';

  return (
    <div className={className}>
      <button
        ref={triggerRef}
        id={id}
        type="button"
        aria-label={ariaLabel}
        aria-haspopup="menu"
        aria-expanded={open}
        disabled={disabled}
        onClick={(event) => {
          event.stopPropagation();
          setOpen((current) => !current);
        }}
        className={triggerClass}
      >
        <span className="truncate">{label}</span>
        <span aria-hidden="true" className="text-text-muted">
          ▾
        </span>
      </button>
      {open &&
        createPortal(
          <div
            ref={panelRef}
            popover={typeof HTMLElement.prototype.showPopover === 'function' ? 'manual' : undefined}
            data-testid="spawn-option-picker-menu"
            data-dropdown-for={menuKey}
            style={{
              position: 'fixed',
              margin: 0,
              right: 'auto',
              bottom: 'auto',
              left: position.left,
              top: position.top,
              width: position.width,
              maxHeight: 'min(60vh, calc(100vh - 16px))',
              zIndex: 1000,
            }}
            className="overflow-y-auto rounded-md border border-border-default bg-bg-overlay shadow-md"
            onClick={(event) => event.stopPropagation()}
            onKeyDown={(event) => {
              if (event.key !== 'Escape') return;
              event.preventDefault();
              event.stopPropagation();
              close();
            }}
          >
            <div role="menu" aria-label={ariaLabel}>
              <button
                type="button"
                role="menuitem"
                className={unsetClass}
                onClick={() => pick(unsetValue)}
              >
                {unsetLabel}
              </button>
            </div>
            <div className="border-t border-border-subtle">
              <GroupedProviderMenu
                providers={options}
                filter={filter}
                onSelect={(providerId, _altKey, configurationId) => pick(configurationId ?? providerId)}
                onClose={close}
              />
            </div>
          </div>,
          triggerRef.current?.closest('[role="dialog"]') ?? document.body,
        )}
    </div>
  );
}
