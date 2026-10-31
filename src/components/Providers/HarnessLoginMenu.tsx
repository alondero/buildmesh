import { useEffect, useLayoutEffect, useRef, useState } from 'react';
import { createPortal } from 'react-dom';
import type { SpawnOption } from '../../lib/groups';
import type { HarnessLoginStatus } from '../../lib/tauri/provider';
import { SpawnConfigurationMenu } from './SpawnConfigurationMenu';
import { placeFlyout, type FlyoutPlacement } from './flyoutPlacement';

/** Why an additional login cannot launch yet, or `undefined` when it can. */
export function loginUnavailableReason(login: HarnessLoginStatus): string | undefined {
  if (login.is_default || login.signed_in) return undefined;
  return 'Not signed in. Sign in from Settings › Launch Configurations.';
}

/**
 * The login submenu: sits between the Codex harness row and its Launch
 * Configurations when more than one Codex login is configured. Picking a
 * login spawns Codex's defaults with it; its disclosure opens the
 * configurations offered under that login.
 */
export function HarnessLoginMenu({ option, anchor, keyboard, logins, configurationRows, decorate, onSelect, onClose, onDismiss, onEditingChange }: {
  option: SpawnOption;
  anchor: HTMLElement;
  keyboard: boolean;
  logins: HarnessLoginStatus[];
  configurationRows: SpawnOption[];
  decorate?: (option: SpawnOption) => SpawnOption;
  onSelect: (providerId: string, altKey: boolean, configurationId?: string, loginId?: string) => void;
  onClose: () => void;
  onDismiss: () => void;
  onEditingChange: (editing: boolean) => void;
}) {
  const panel = useRef<HTMLDivElement>(null);
  const editing = useRef(false);
  const [placement, setPlacement] = useState<FlyoutPlacement | null>(null);
  const [active, setActive] = useState(0);
  const [submenu, setSubmenu] = useState<{ login: HarnessLoginStatus; anchor: HTMLElement; keyboard: boolean } | null>(null);

  useLayoutEffect(() => {
    panel.current?.showPopover?.();
    const place = () => {
      const rect = anchor.getBoundingClientRect();
      if (rect.width === 0 && rect.height === 0) return;
      const next = placeFlyout(rect, panel.current?.offsetWidth ?? 240, panel.current?.scrollHeight ?? 0,
        window.innerWidth, window.innerHeight);
      setPlacement((current) => current && current.left === next.left && current.top === next.top
        && current.maxHeight === next.maxHeight ? current : next);
    };
    place();
    window.addEventListener('resize', place);
    window.addEventListener('scroll', place, true);
    return () => {
      window.removeEventListener('resize', place);
      window.removeEventListener('scroll', place, true);
    };
  }, [anchor, logins]);

  useEffect(() => {
    if (keyboard) panel.current?.querySelector<HTMLElement>('[data-login-id]')?.focus();
  }, [keyboard]);

  const close = () => { onClose(); anchor.focus({ preventScroll: true }); };
  const rowClass = (index: number) => `min-w-0 flex-1 px-3 py-1.5 text-left text-xs focus:outline-none ${
    index === active ? 'bg-bg-selection text-text-primary' : 'text-text-primary'}`;

  return createPortal(
    <div
      ref={panel}
      popover={typeof HTMLElement.prototype.showPopover === 'function' ? 'manual' : undefined}
      data-dropdown-for={anchor.closest('[data-dropdown-for]')?.getAttribute('data-dropdown-for') ?? undefined}
      data-testid="harness-logins"
      style={{
        position: 'fixed', margin: 0, right: 'auto', bottom: 'auto',
        left: placement?.left ?? 0,
        top: placement?.top ?? 0,
        width: 'min(240px, calc(100vw - 16px))',
        maxHeight: placement ? `${placement.maxHeight}px` : 'calc(100vh - 16px)',
        zIndex: 1000,
      }}
      className="overflow-y-auto rounded-md border border-border-default bg-bg-overlay shadow-md"
      onClick={(e) => e.stopPropagation()}
      onKeyDown={(e) => {
        e.stopPropagation();
        // Keys from the configuration submenu (a portal child) are its own.
        if (!panel.current?.contains(e.target as Node)) return;
        if (e.key === 'Tab') { e.preventDefault(); onDismiss(); return; }
        if (e.key === 'Escape' || e.key === 'ArrowLeft') {
          e.preventDefault();
          // A hover-opened submenu leaves focus on the login row: close the
          // submenu first, then the login menu on the next press.
          if (submenu) setSubmenu(null); else close();
          return;
        }
        const items = Array.from(panel.current?.querySelectorAll<HTMLElement>('[data-login-id]') ?? []);
        const index = items.indexOf(document.activeElement as HTMLElement);
        const next = e.key === 'ArrowDown' ? (index + 1) % items.length
          : e.key === 'ArrowUp' ? (index - 1 + items.length) % items.length
          : e.key === 'Home' ? 0 : e.key === 'End' ? items.length - 1 : null;
        if (next !== null) { e.preventDefault(); items[next]?.focus(); }
      }}
    >
      <div role="menu" aria-label={`${option.label} logins`}>
        {logins.map((login, index) => {
          const unavailable = loginUnavailableReason(login);
          return (
            <div key={login.id} role="presentation" className="flex"
              onMouseEnter={(e) => {
                if (editing.current) return;
                const row = e.currentTarget.querySelector<HTMLElement>('[data-login-id]');
                row?.focus({ preventScroll: true });
                setSubmenu(row && !unavailable ? { login, anchor: row, keyboard: false } : null);
              }}
            >
              <button type="button" role="menuitem" data-login-id={login.id}
                tabIndex={index === active ? 0 : -1}
                aria-haspopup={unavailable ? undefined : 'menu'}
                aria-expanded={unavailable ? undefined : submenu?.login.id === login.id}
                aria-disabled={Boolean(unavailable)}
                title={unavailable ?? login.email ?? undefined}
                className={rowClass(index)}
                onFocus={() => setActive(index)}
                onClick={(e) => { if (!unavailable) onSelect(option.id, e.altKey, undefined, login.id); }}
                onKeyDown={(e) => {
                  if (e.key === 'ArrowRight' && !unavailable) {
                    e.preventDefault(); e.stopPropagation();
                    setSubmenu({ login, anchor: e.currentTarget, keyboard: true });
                  } else if (e.key === 'ArrowUp' || e.key === 'ArrowDown') {
                    setSubmenu(null);
                  }
                }}
              >
                <span className="block truncate">{login.label}</span>
                {login.email && <span className="block truncate text-text-secondary">{login.email}</span>}
                {/* The full reason (where to sign in) rides the row's title. */}
                {unavailable && <span className="block text-text-muted">Not signed in</span>}
                {login.same_account_as && <span className="block whitespace-normal text-status-warning">Same ChatGPT account as {login.same_account_as}</span>}
              </button>
              {!unavailable && <button type="button" tabIndex={-1} aria-label={`${login.label} configurations`}
                aria-haspopup="menu" aria-expanded={submenu?.login.id === login.id}
                className="px-2 text-xs text-text-secondary hover:bg-bg-selection"
                onClick={(e) => {
                  const row = e.currentTarget.previousElementSibling as HTMLElement;
                  setSubmenu({ login, anchor: row, keyboard: true });
                }}
              >›</button>}
            </div>
          );
        })}
      </div>
      {submenu && <SpawnConfigurationMenu key={submenu.login.id} option={option} anchor={submenu.anchor}
        keyboard={submenu.keyboard} login={{ id: submenu.login.id, label: submenu.login.label }}
        configurationRows={configurationRows} decorate={decorate}
        onEditingChange={(value) => { editing.current = value; onEditingChange(value); }}
        onSelect={onSelect} onClose={() => setSubmenu(null)}
        onDismiss={() => { setSubmenu(null); onDismiss(); }} />}
    </div>,
    anchor.closest('[role="dialog"]') ?? document.body,
  );
}
