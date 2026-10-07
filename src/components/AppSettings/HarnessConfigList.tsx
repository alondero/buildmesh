import * as api from '../../lib/tauri';
import type {
  ProviderAccount,
  ProviderPairing,
  PairingVerification,
  ModelTiers,
} from '../../lib/tauri';
import { HarnessCard } from './HarnessCard';

/** A harness that can host Proxied Providers (speaks a Compatible API surface). */
export interface ProxyHarness {
  id: string;
  label: string;
}

// Public surface preserved across the issue #1880 split: both names were
// exported from this module before, and consumers (HarnessesPane, the unit
// test) still import them from here. `MODEL_TIER_FIELDS` now lives with the
// other shared vocabulary so `HarnessCard` and `ProxiedChildRow` don't have
// to import each other.
export { MODEL_TIER_FIELDS } from './harnessVocabulary';
export { reorderProxiedIds } from './HarnessCard';

/**
 * Harness-centric config (ADR-0016 §5 / ADR-0025). Attach collects base URL
 * (first-class prefilled) and Anthropic model tiers; inline edit after attach.
 */
export function HarnessConfigList({
  harnesses,
  compatibleByHarness,
  pairings,
  verifications = [],
  supportsWsl = true,
  storedKeys,
  accounts,
  onAttach,
  onUpdate,
  onDetach,
  onVerify = async () => {},
  onReorderProxied,
  onDirtyChange,
  disabled = false,
}: {
  harnesses: ProxyHarness[];
  compatibleByHarness: Record<string, ProviderAccount[]>;
  pairings: ProviderPairing[];
  verifications?: PairingVerification[];
  supportsWsl?: boolean;
  storedKeys: Set<string>;
  accounts: ProviderAccount[];
  onAttach: (
    harnessId: string,
    providerId: string,
    apiKey: string | null,
    baseUrl: string | null,
    modelTiers: ModelTiers | null,
  ) => Promise<void>;
  onUpdate?: (
    harnessId: string,
    providerId: string,
    baseUrl: string | null,
    modelTiers: ModelTiers | null,
  ) => Promise<void>;
  onDetach: (harnessId: string, providerId: string) => Promise<void>;
  onVerify?: (harnessId: string, providerId: string, envType: api.EnvType) => Promise<void>;
  onReorderProxied?: (harnessId: string, providerIds: string[]) => void;
  onDirtyChange?: (harnessId: string, dirty: boolean) => void;
  /** Issue #1523 — every attach / edit / detach here writes
   *  `provider_pairings` in `preferences.json`, which the backend refuses
   *  while the file is corrupt. Folded into `busy` so the whole list — rows
   *  and the attach form — locks from one gate. */
  disabled?: boolean;
}) {
  const accountName = (id: string) => accounts.find((a) => a.id === id)?.name ?? id;
  const isKeyed = (id: string) => {
    const k = accounts.find((a) => a.id === id)?.api_key;
    return !!k && k.length > 0;
  };

  const visible = harnesses.filter(
    (h) =>
      (compatibleByHarness[h.id]?.length ?? 0) > 0 ||
      pairings.some((p) => p.harness_id === h.id),
  );

  if (visible.length === 0) {
    return (
      <p className="text-base text-text-muted">
        No harnesses on this host can host a proxied provider yet.
      </p>
    );
  }

  return (
    <div className="space-y-4" data-testid="harness-config-list">
      {visible.map((harness) => (
        <HarnessCard
          key={harness.id}
          harness={harness}
          compatible={compatibleByHarness[harness.id] ?? []}
          pairings={pairings.filter((p) => p.harness_id === harness.id)}
          verifications={verifications.filter((v) => v.harness_id === harness.id)}
          supportsWsl={supportsWsl}
          storedKeys={storedKeys}
          accountName={accountName}
          isKeyed={isKeyed}
          onAttach={onAttach}
          onUpdate={onUpdate}
          onDetach={onDetach}
          onVerify={onVerify}
          onReorderProxied={onReorderProxied}
          onDirtyChange={onDirtyChange ? (d) => onDirtyChange(harness.id, d) : undefined}
          disabled={disabled}
        />
      ))}
    </div>
  );
}
