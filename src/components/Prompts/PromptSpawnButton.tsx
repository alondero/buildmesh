import { useRef, useState } from 'react';
import type { SpawnOption } from '../../lib/groups';
import { getDefaultProvider } from '../../lib/tauri/provider';
import { useClickOutside } from '../../hooks/useClickOutside';
import { SpawnButtonCluster } from '../Sidebar/SpawnButtonCluster';
import type { Mesh } from '../../types/generated/Mesh';
import { PromptMeshPicker } from './PromptMeshPicker';

interface MenuState {
  meshId: number | null;
  providersOpen: boolean;
  pending: { providerId?: string; configurationId?: string; loginId?: string } | null;
}

export function PromptSpawnButton({ meshId, meshes, promptId, name, providers, busy, disabled, portalMenu = true, onSpawn }: {
  meshId: number | null;
  meshes: Mesh[];
  promptId: string;
  name: string;
  providers: SpawnOption[];
  busy: boolean;
  disabled: boolean;
  portalMenu?: boolean;
  onSpawn: (providerId?: string, configurationId?: string, loginId?: string, mesh?: Mesh) => void | Promise<void>;
}) {
  const [menu, setMenu] = useState<MenuState>({ meshId, providersOpen: false, pending: null });
  // A launch choice must not survive losing eligibility or changing context.
  if (menu.meshId !== meshId || ((disabled || busy) && (menu.providersOpen || menu.pending))) {
    setMenu({ meshId, providersOpen: false, pending: null });
  }
  const { providersOpen: open, pending } = menu;
  const root = useRef<HTMLDivElement>(null);
  const key = `prompt-${meshId ?? 'none'}-${promptId}`;
  const close = () => setMenu({ meshId, providersOpen: false, pending: null });
  const focusTrigger = () => root.current?.querySelector<HTMLButtonElement>('[data-testid="spawn-default"]')?.focus();
  const dismissPicker = () => {
    close(); focusTrigger();
  };
  const requestSpawn = (providerId?: string, configurationId?: string, loginId?: string) => {
    if (meshId === null) setMenu({ meshId, providersOpen: false, pending: { providerId, configurationId, loginId } });
    else { close(); return onSpawn(providerId, configurationId, loginId); }
  };
  const spawnInMesh = async (mesh: Mesh) => {
    if (!pending) return;
    close();
    // The primary button is disabled during launch and remounts when its
    // default Mesh changes. Keep focus in the stable group until it settles.
    root.current?.focus();
    try { await onSpawn(pending.providerId, pending.configurationId, pending.loginId, mesh); }
    finally {
      requestAnimationFrame(() => {
        if (root.current && document.activeElement === root.current) focusTrigger();
      });
    }
  };
  useClickOutside(open || pending ? key : null, close);
  return <div ref={root} role="group" aria-label={`Launch ${name}`} tabIndex={-1} data-dropdown-for={key}
    className="min-w-0 max-w-full rounded-sm focus-visible:outline focus-visible:outline-2 focus-visible:outline-accent-cyan" onKeyDown={event => {
    // Close before an enclosing modal's Tab trap moves focus out of the menu.
    // Leave the default Tab action and the modal's focus handling intact.
    if (event.key === 'Tab' && pending) { close(); return; }
    // Escape dismisses the picker before an enclosing customisation dialog.
    if (event.key === 'Escape' && (open || pending) && !event.nativeEvent.isComposing) {
      event.preventDefault(); event.stopPropagation();
      if (pending) dismissPicker();
      else {
        close();
        root.current?.querySelector<HTMLButtonElement>('[aria-haspopup="menu"]')?.focus();
      }
    }
  }}>
    <SpawnButtonCluster key={meshId ?? 'none'} providers={providers} dropdownKey={key}
      primaryAriaLabel={`Spawn ${name}`} isOpen={open}
      onToggleDropdown={() => setMenu(current => ({ meshId, providersOpen: !current.providersOpen, pending: null }))} portalMenu={portalMenu}
      isSpawning={busy} disabled={disabled} getDefaultProvider={meshId === null ? undefined : () => getDefaultProvider(meshId)}
      onSpawnDefault={() => requestSpawn()}
      onSelectProvider={(providerId, _altKey, configurationId, loginId) => {
        requestSpawn(providerId, configurationId, loginId);
      }} />
    {pending && !disabled && !busy && <PromptMeshPicker meshes={meshes} anchorRef={root} dropdownKey={key} onClose={close}
      onSelect={mesh => void spawnInMesh(mesh)} />}
  </div>;
}
