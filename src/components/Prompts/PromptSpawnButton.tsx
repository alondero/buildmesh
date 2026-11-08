import { useRef, useState } from 'react';
import type { SpawnOption } from '../../lib/groups';
import { getDefaultProvider } from '../../lib/tauri/provider';
import { useClickOutside } from '../../hooks/useClickOutside';
import { SpawnButtonCluster } from '../Sidebar/SpawnButtonCluster';

export function PromptSpawnButton({ meshId, promptId, name, providers, busy, disabled, portalMenu = true, onSpawn }: {
  meshId: number | null;
  promptId: string;
  name: string;
  providers: SpawnOption[];
  busy: boolean;
  disabled: boolean;
  portalMenu?: boolean;
  onSpawn: (providerId?: string, configurationId?: string, loginId?: string) => void;
}) {
  const [open, setOpen] = useState(false);
  const root = useRef<HTMLDivElement>(null);
  const key = `prompt-${meshId ?? 'none'}-${promptId}`;
  useClickOutside(open ? key : null, () => setOpen(false));
  return <div ref={root} className="min-w-0 max-w-full" onKeyDown={event => {
    // Escape dismisses the picker before an enclosing customisation dialog.
    if (event.key === 'Escape' && open && !event.nativeEvent.isComposing) {
      event.preventDefault(); event.stopPropagation(); setOpen(false);
      root.current?.querySelector<HTMLButtonElement>('[aria-haspopup="menu"]')?.focus();
    }
  }}>
    <SpawnButtonCluster providers={providers} dropdownKey={key} primaryLabel="Spawn"
      primaryAriaLabel={`Spawn ${name}`} isOpen={open} onToggleDropdown={() => setOpen(value => !value)} portalMenu={portalMenu}
      isSpawning={busy} disabled={disabled} getDefaultProvider={meshId === null ? undefined : () => getDefaultProvider(meshId)}
      onSpawnDefault={() => { setOpen(false); onSpawn(); }}
      onSelectProvider={(providerId, _altKey, configurationId, loginId) => {
        setOpen(false); onSpawn(providerId, configurationId, loginId);
      }} />
  </div>;
}
