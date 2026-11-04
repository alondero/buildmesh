import type { SpawnOption } from '../../lib/groups';
import { GroupedProviderMenu } from './GroupedProviderMenu';

import type { RegenerateSelection } from '../../lib/regenerate';
import type { SpawnConfiguration } from '../../types/generated/SpawnConfiguration';

export interface RegenerateProviderMenuProps {
  providers: SpawnOption[];
  currentProviderId: string;
  currentConfiguration?: SpawnConfiguration;
  onPick: (providerId: string, providerLabel: string, selection?: RegenerateSelection) => void;
  submenuTestId?: string;
  onClose?: () => void;
}

/** Regenerate adds Current to the shared Spawn Menu's navigation and ladder. */
export function RegenerateProviderMenu({ providers, currentProviderId, currentConfiguration, onPick, onClose, submenuTestId = 'regenerate-submenu' }: RegenerateProviderMenuProps) {
  const current = providers.find((option) => option.id === currentProviderId) ?? (currentConfiguration ? {
    id: currentConfiguration.id,
    label: currentConfiguration.name,
    harness_id: currentConfiguration.resolved?.harness.id ?? currentConfiguration.spawn_option_id.split(':')[0],
    provider_id: null, is_proxied: false, group_key: '', color: '', icon: '',
  } : undefined);
  if (!providers.length) return <div data-testid={`${submenuTestId}-empty`} className="px-3 py-1.5 text-xs text-text-muted">No providers available</div>;
  return <GroupedProviderMenu providers={providers} onClose={onClose}
    leadingAction={current ? {
      id: current.id,
      current: true,
      option: current,
      label: `Current (${current.label})`,
      title: 'Regenerate in place using the current launch configuration and login',
      testId: `${submenuTestId}-current`,
      onSelect: () => onPick(currentConfiguration?.spawn_option_id ?? current.id, current.label),
    } : undefined}
    onSelect={(providerId, _altKey, configurationId, loginId) => {
      const option = providers.find((row) => row.id === (configurationId ?? providerId))
        ?? providers.find((row) => row.id === providerId);
      onPick(providerId, option?.label ?? providerId, { configurationId, loginId, replaceSelection: true });
    }}
  />;
}
