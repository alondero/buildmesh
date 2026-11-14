import { useRef, useState } from 'react';
import type { SpawnOption } from '../../lib/groups';
import { getDefaultProvider } from '../../lib/tauri/provider';
import { useClickOutside } from '../../hooks/useClickOutside';
import { SpawnButtonCluster } from '../Sidebar/SpawnButtonCluster';
import type { Mesh } from '../../types/generated/Mesh';
import { PromptMeshPicker } from './PromptMeshPicker';

export function PromptSpawnButton({ meshId, meshes = [], promptId, name, providers, busy, disabled, portalMenu = true, onSpawn }: {
  meshId: number | null;
  meshes?: Mesh[];
  promptId: string;
  name: string;
  providers: SpawnOption[];
  busy: boolean;
  disabled: boolean;
  portalMenu?: boolean;
  onSpawn: (providerId?: string, configurationId?: string, loginId?: string, mesh?: Mesh) => void;
}) {
  const [open, setOpen] = useState(false);
  const [pending, setPending] = useState<{ providerId?: string; configurationId?: string; loginId?: string } | null>(null);
  const root = useRef<HTMLDivElement>(null);
  const key = `prompt-${meshId ?? 'none'}-${promptId}`;
  const close = () => { setOpen(false); setPending(null); };
  const dismissPicker = () => {
    close();
    root.current?.querySelector<HTMLButtonElement>('[data-testid="spawn-default"]')?.focus();
  };
  const requestSpawn = (providerId?: string, configurationId?: string, loginId?: string) => {
    setOpen(false);
    if (meshId === null) setPending({ providerId, configurationId, loginId });
    else onSpawn(providerId, configurationId, loginId);
  };
  useClickOutside(open || pending ? key : null, close);
  return <div ref={root} data-dropdown-for={key} className="min-w-0 max-w-full" onKeyDown={event => {
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
    <SpawnButtonCluster providers={providers} dropdownKey={key}
      primaryAriaLabel={`Spawn ${name}`} isOpen={open} onToggleDropdown={() => { setPending(null); setOpen(value => !value); }} portalMenu={portalMenu}
      isSpawning={busy} disabled={disabled} getDefaultProvider={meshId === null ? undefined : () => getDefaultProvider(meshId)}
      onSpawnDefault={() => requestSpawn()}
      onSelectProvider={(providerId, _altKey, configurationId, loginId) => {
        requestSpawn(providerId, configurationId, loginId);
      }} />
    {pending && !disabled && <PromptMeshPicker meshes={meshes} anchorRef={root} dropdownKey={key} onClose={dismissPicker}
      onSelect={mesh => { close(); onSpawn(pending.providerId, pending.configurationId, pending.loginId, mesh); }} />}
  </div>;
}
