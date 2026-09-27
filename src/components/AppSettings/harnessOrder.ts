import type { ProviderInfo } from '../../lib/tauri';

/** Returns unique, reorderable harness rows, preserving the first parent row. */
export function getOrderableHarnesses(providers: ProviderInfo[]): ProviderInfo[] {
  const seen = new Set<string>();
  const harnesses: ProviderInfo[] = [];

  for (const provider of providers) {
    if (provider.configuration || provider.is_proxied || provider.harness_id === 'terminal') continue;
    if (seen.has(provider.harness_id)) continue;

    seen.add(provider.harness_id);
    harnesses.push({ ...provider, id: provider.harness_id });
  }

  return harnesses;
}
