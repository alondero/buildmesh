import { useState } from 'react';
import type { Provider } from '../../api';
import { launchConfigurationApi } from '../../api';

import { ProviderIcon } from '../../../components/Providers/ProviderIcon';
import { Sheet } from '../../ui';
import { launchChoices, launchGroups } from '../../spawnMenu';
import { LaunchConfigurations } from '../../../components/Providers/LaunchConfigurations';

export function ProviderPicker({
  providers,
  onChanged,
  onPick,
  onCancel,
}: {
  providers: Provider[];
  onChanged: () => void;
  onPick: (providerId: string, configurationId?: string, loginId?: string) => void;
  onCancel: () => void;
}) {
  const [managing, setManaging] = useState(false);
  const choices = launchChoices(providers, { needsPrompt: false, includeUnavailable: true });

  return (
    <Sheet onClose={onCancel} testId="provider-picker" label="New Agent Node">
      <button type="button" className="btn-ghost" onClick={onCancel}>Cancel</button>
      <h3
        style={{
          fontSize: 15,
          fontWeight: 600,
          color: "var(--text)",
          margin: 0,
          marginBottom: 14,
        }}
      >
        New Agent Node
      </h3>
      <button className="card" onClick={() => setManaging((value) => !value)}>
        {managing ? "Back to launch choices" : "Manage Launch Configurations"}
      </button>
      {managing && (
        <LaunchConfigurations
          api={launchConfigurationApi}
          onChanged={onChanged}
        />
      )}
      {!managing && (
        <>
          {launchGroups(choices).map(({ id, label, choices: rows }) => <section key={id} aria-label={label}>
            <h4>{label}</h4>
            {rows.map((choice) => {
              const provider = providers.find((row) => row.id === choice.provider);
              return <button type="button" key={choice.id} className="spawn-menu-item"
                data-testid={`${choice.configurationId ? 'configuration' : 'provider'}-${choice.id}`} disabled={!!choice.unavailableReason}
                onClick={() => onPick(choice.provider, choice.configurationId, choice.loginId)}>
                {provider && <ProviderIcon providerId={provider.provider_id ?? provider.harness_id}
                  withBackground backgroundColor={provider.color} fallbackGlyph={provider.icon}
                  chipTestId={`picker-avatar-${choice.id}`} title={provider.label} className="h-4 w-4" />}
                {choice.label}
                {choice.unavailableReason && <small>{choice.unavailableReason}</small>}
                {choice.warning && <small>{choice.warning}</small>}
              </button>;
            })}
          </section>)}
        </>
      )}
    </Sheet>
  );
}
