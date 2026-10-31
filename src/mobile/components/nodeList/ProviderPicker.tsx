import { useState } from 'react';
import type { Provider } from '../../api';
import { launchConfigurationApi } from '../../api';

import { ProviderIcon } from '../../../components/Providers/ProviderIcon';
import { Sheet } from '../../ui';
import { groupByHarness } from '../../../lib/groups';
import { LaunchConfigurations } from '../../../components/Providers/LaunchConfigurations';

export function ProviderPicker({
  providers,
  onChanged,
  onPick,
  onCancel,
}: {
  providers: Provider[];
  onChanged: () => void;
  onPick: (providerId: string, configurationId?: string) => void;
  onCancel: () => void;
}) {
  const [managing, setManaging] = useState(false);
  // The spawn picker shows harness parents and their saved launch recipes.
  // Direct provider routes remain in the backend list for route selectors,
  // but do not become flat spawn choices even after all recipes are deleted.
  const groups = groupByHarness(providers).filter(([, group]) =>
    group.some((row) => !row.is_proxied || Boolean(row.configuration)),
  );

  const configurationsFor = (providerLabel: string, group: Provider[]) => {
    const recipes = group
      .filter((row) => row.configuration)
      .map((row) => ({ row, configuration: row.configuration! }));
    return recipes.length ? (
      <details style={{ marginLeft: 18, marginBottom: 8 }}>
        <summary
          style={{ padding: 10, color: "var(--text-dim)", fontSize: 13 }}
        >
          {providerLabel} configurations
        </summary>
        {recipes.map(({ row, configuration }) => (
          <button
            type="button"
            className="card"
            key={configuration.id}
            disabled={Boolean(row.unavailable_reason)}
            onClick={() =>
              onPick(configuration.spawn_option_id, configuration.id)
            }
          >
            {configuration.name}
            {row.unavailable_reason && (
              <small style={{ display: "block" }}>
                {row.unavailable_reason}
              </small>
            )}
            {row.verification_warning && (
              <small style={{ display: "block", color: "var(--amber)" }}>
                {row.verification_warning}
              </small>
            )}
          </button>
        ))}
      </details>
    ) : null;
  };

  return (
    <Sheet onClose={onCancel} testId="provider-picker">
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
          {groups.map(([harnessId, group]) => {
            const native = group.find(
              (row) => !row.is_proxied && !row.configuration,
            );
            const harnessLabel =
              native?.label ??
              group.find((row) => row.configuration)?.configuration
                ?.harness_id ??
              harnessId;
            return (
              <div
                key={harnessId}
                data-testid={`spawn-group-${harnessId}`}
                style={{ marginBottom: 8 }}
              >
                {native ? (
                  <button
                    type="button"
                    onClick={() => onPick(native.id)}
                    disabled={Boolean(native.unavailable_reason)}
                    data-testid={`provider-${native.id}`}
                    className="card"
                    style={{ background: "var(--surface-2)" }}
                  >
                    <ProviderIcon
                      // `fallbackGlyph` feeds the row's own wire letter in for a
                      // harness profile with no brand mark (issue #1086), the same
                      // way `NodeRow` does for a custom Proxied account (#948).
                      // Here the row IS the live `listProviders()` record, so
                      // `native.icon` is the value `NodeRow` has to look up — and
                      // it keeps the chip's glyph on the same source as its colour.
                      providerId={native.provider_id ?? native.harness_id}
                      withBackground
                      backgroundColor={native.color}
                      fallbackGlyph={native.icon}
                      chipTestId={`picker-avatar-${native.id}`}
                      title={native.label}
                      className="h-4 w-4"
                    />
                    <span
                      style={{ flex: 1, fontSize: 15, color: "var(--text)" }}
                    >
                      {native.label}
                      {native.unavailable_reason && (
                        <small style={{ display: "block" }}>
                          {native.unavailable_reason}
                        </small>
                      )}
                      {native.verification_warning && (
                        <small style={{ display: "block", color: "var(--amber)" }}>
                          {native.verification_warning}
                        </small>
                      )}
                    </span>
                  </button>
                ) : (
                  <p role="heading" aria-level={4} className="card">
                    {harnessLabel}
                  </p>
                )}
                {configurationsFor(harnessLabel, group)}
              </div>
            );
          })}
        </>
      )}
    </Sheet>
  );
}
