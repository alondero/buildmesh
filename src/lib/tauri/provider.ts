//! Provider / harness IPC facet (issue #1656).
//!
//! The first facet of the `tauri/` directory split. Owns every typed
//! wrapper around the harness + provider + pairing + usage Tauri commands.
//! Routes through the `_invoke` chokepoint (ADR-0010) defined in
//! `./_invoke.ts` — *not* through raw `@tauri-apps/api/core`, which is
//! the seam-test enforcement point.
//!
//! ## What lives here
//!
//! - **Harness defaults** — `setHarnessDefault`, `clearHarnessDefault`,
//!   `upsertMeshHarnessOverride`, `removeMeshHarnessOverride`,
//!   `clearMeshHarnessOverrides`, `setHarnessOrder`,
//!   `setProxiedProviderOrder` (each pair-write busts the
//!   `providerListPromise` cache so a stale menu never lingers).
//! - **Provider catalogue** — `listProviders` (module-scope promise cache
//!   via `providerCache.ts`), `getDefaultProvider` (per-mesh promise
//!   cache).
//! - **Provider accounts** — `getProviderAccounts`,
//!   `getKeyedFirstClassCatalog`, `upsertProviderAccount`,
//!   `removeProviderAccount`.
//! - **Proxied provider pairings** — `getProviderPairings`,
//!   `getPairingVerifications`, `verifyProviderPairing`,
//!   `getPairingDefaults`, `compatibleProvidersForHarness`,
//!   `attachProxiedProvider`, `updateProviderPairing`,
//!   `removeProviderPairing`.
//! - **Usage meters** — `getProviderMeters`, `getMuseSessionTelemetry`
//!   + the `MUSE_SESSION_TELEMETRY_EVENT` constant.
//! - **Resolved harness view** (issue #1656) — `getResolvedHarnessView`
//!   returns the per-harness cascade (four-layer breakdown +
//!   capability-masked resolved value) so the UI reads the cascade
//!   rather than re-implementing it client-side.
//!
//! ## Why this is the first facet
//!
//! `provider.ts` is the natural home for the new `getResolvedHarnessView`
//! IPC command (issue #1656 Phase 1) and owns every read+write to the
//! `AppPreferences.harness_defaults` + `meshes.harness_overrides` data.
//! The cache-bust discipline (`finally { setProviderListPromise(null); }`)
//! is co-located with the wrapper that triggers it — moving the
//! wrappers here keeps the discipline intact.
//!
//! ## Co-location invariant
//!
//! Every `finally { setProviderListPromise(null) }` block in this file
//! is co-located with the wrapper that triggers it. Future cache-busting
//! wrappers MUST stay in this file (or a sibling facet) — moving one
//! to the root `tauri.ts` would re-introduce the split-discipline risk
//! the facet refactor was meant to eliminate.

import {
  deleteDefaultProviderPromise,
  getDefaultProviderPromise,
  getProviderListPromise,
  resetProviderCachesForTests,
  setDefaultProviderPromise,
  setProviderListPromise,
} from '../providerCache';
import { _invoke } from './_invoke';
// Wire types consumed by the wrappers below. `import type` so the
// ts-rs-generated bindings are erased at compile time — every entry
// here is purely a compile-time type. The runtime payload objects are
// produced by `_invoke<T>(...)` via the bridge.
import type { ProviderInfo } from '../../types/generated/ProviderInfo';
import type { HarnessConfigValue } from '../../types/generated/HarnessConfigValue';
import type { ProviderAccount } from '../../types/generated/ProviderAccount';
import type { EnvType } from '../../types/generated/EnvType';
import type { PairingVerification } from '../../types/generated/PairingVerification';
import type { ProviderPairing } from '../../types/generated/ProviderPairing';
import type { ModelTiers } from '../../types/generated/ModelTiers';
import type { ProviderMeters } from '../../types/generated/ProviderMeters';
import type { ObservedMuseSessionTelemetry } from '../../types/generated/ObservedMuseSessionTelemetry';
import type { ResolvedHarnessView } from '../../types/generated/ResolvedHarnessView';
import type { ResolvedCascadeView } from '../../types/generated/ResolvedCascadeView';
import type { ResolvedCascadeLayer } from '../../types/generated/ResolvedCascadeLayer';
import type { CapabilityMaskForResolver } from '../../types/generated/CapabilityMaskForResolver';
import type { BillingMode } from '../../types/generated/BillingMode';
import type { ApiSurface } from '../../types/generated/ApiSurface';
import type { UsageWindow } from '../../types/generated/UsageWindow';
import type { UsageAmount } from '../../types/generated/UsageAmount';
import type { UsageMeter } from '../../types/generated/UsageMeter';
import type { ProviderUsage } from '../../types/generated/ProviderUsage';
import type { BillingBalance } from '../../types/generated/BillingBalance';

// Re-export so consumers of `lib/tauri` (the facade) can keep importing
// types from the same place they used to. `export type` is the right
// shape here because the underlying imports are `import type` — TypeScript
// preserves `export type` re-exports through the facade's `export *`.
// The list below mirrors the import list above; add names to BOTH lists
// when extending the facet's wire-type surface.
export type {
  ProviderInfo,
  HarnessConfigValue,
  ProviderAccount,
  BillingMode,
  ModelTiers,
  ApiSurface,
  EnvType,
  ProviderPairing,
  PairingVerification,
  UsageWindow,
  UsageAmount,
  UsageMeter,
  ProviderUsage,
  BillingBalance,
  ProviderMeters,
  ObservedMuseSessionTelemetry,
  ResolvedCascadeView,
  ResolvedCascadeLayer,
  CapabilityMaskForResolver,
  ResolvedHarnessView,
};

/**
 * Test-only: clear the module-level provider caches between cases. Exported
 * with a leading-underscore name so accidental production use is loud.
 * Re-exported by the `tauri.ts` facade so existing test imports
 * (`import { __resetProviderCachesForTests } from '../../src/lib/tauri'`)
 * continue to work post-facet split.
 */
export function __resetProviderCachesForTests(): void {
  resetProviderCachesForTests();
}

// ----- Provider catalogue ------------------------------------------------

/** Per-mesh provider lookup. Cached by mesh id (issue #405). */
export const getDefaultProvider = (meshId: number): Promise<string> => {
  const existing = getDefaultProviderPromise(meshId);
  if (existing) return existing;
  const next = _invoke<string>('get_default_provider', { meshId })
    .catch((e) => {
      deleteDefaultProviderPromise(meshId);
      throw e;
    }) as Promise<string>;
  setDefaultProviderPromise(meshId, next);
  return next;
};

/** Global provider list. Cached module-scope promise (issue #405). */
export const listProviders = (): Promise<
  ProviderInfo[]
> => {
  const existing = getProviderListPromise();
  if (existing) return existing;
  const next = _invoke<ProviderInfo[]>(
    'list_providers',
  ).catch((e) => {
    setProviderListPromise(null);
    throw e;
  }) as Promise<
    ProviderInfo[]
  >;
  setProviderListPromise(next);
  return next;
};

// ----- Harness defaults --------------------------------------------------

export const setHarnessDefault = (
  profileId: string,
  value: HarnessConfigValue,
) =>
  _invoke<void>('set_harness_default', {
    profileId,
    value,
  });

export const clearHarnessDefault = (profileId: string) =>
  _invoke<void>('clear_harness_default', { profileId });

export const upsertMeshHarnessOverride = (
  meshId: number,
  harnessId: string,
  value: HarnessConfigValue,
) =>
  _invoke<void>('upsert_mesh_harness_override', {
    meshId,
    harnessId,
    value,
  });

export const removeMeshHarnessOverride = (meshId: number, harnessId: string) =>
  _invoke<void>('remove_mesh_harness_override', { meshId, harnessId });

export const clearMeshHarnessOverrides = (meshId: number) =>
  _invoke<void>('clear_mesh_harness_overrides', { meshId });

/** Reorder the spawn-menu harness rows. Busts the provider-list cache. */
export const setHarnessOrder = async (order: string[]): Promise<void> => {
  try {
    await _invoke<void>('set_harness_order', { order });
  } finally {
    setProviderListPromise(null);
  }
};

/** Reorder the proxied-provider list under one harness. Busts cache. */
export const setProxiedProviderOrder = async (
  harnessId: string,
  providerIds: string[],
): Promise<void> => {
  try {
    await _invoke<void>('set_proxied_provider_order', { harnessId, providerIds });
  } finally {
    setProviderListPromise(null);
  }
};

// ----- Provider accounts -------------------------------------------------

export const getProviderAccounts = () =>
  _invoke<ProviderAccount[]>(
    'get_provider_accounts',
  );

export const getKeyedFirstClassCatalog = () =>
  _invoke<ProviderAccount[]>(
    'get_keyed_first_class_catalog',
  );

/** Upsert a provider account. Busts the provider-list cache. */
export const upsertProviderAccount = async (
  account: ProviderAccount,
): Promise<void> => {
  try {
    await _invoke<void>('upsert_provider_account', { account });
  } finally {
    setProviderListPromise(null);
  }
};

/** Remove a provider account by id. Busts the provider-list cache. */
export const removeProviderAccount = async (id: string): Promise<void> => {
  try {
    await _invoke<void>('remove_provider_account', { id });
  } finally {
    setProviderListPromise(null);
  }
};

// ----- Proxied provider pairings ----------------------------------------

export const getProviderPairings = () =>
  _invoke<ProviderPairing[]>(
    'get_provider_pairings',
  );

export const getPairingVerifications = (
  envType: EnvType = 'windows',
) =>
  _invoke<
    PairingVerification[]
  >('get_pairing_verifications', { envType });

export const verifyProviderPairing = (
  harnessId: string,
  providerId: string,
  envType: EnvType,
) =>
  _invoke<
    PairingVerification
  >('verify_provider_pairing', { harnessId, providerId, envType });

export const getPairingDefaults = (harnessId: string, providerId: string) =>
  _invoke<
    ProviderPairing
  >('get_pairing_defaults', { harnessId, providerId });

export const compatibleProvidersForHarness = (harnessId: string) =>
  _invoke<
    ProviderAccount[]
  >('compatible_providers_for_harness', { harnessId });

/** Attach a proxied provider under a harness. Busts the provider-list cache. */
export const attachProxiedProvider = async (
  harnessId: string,
  providerId: string,
  apiKey: string | null,
  baseUrl: string | null,
  modelTiers: ModelTiers | null,
): Promise<void> => {
  try {
    await _invoke<void>('attach_proxied_provider', {
      harnessId,
      providerId,
      apiKey,
      baseUrl,
      modelTiers,
    });
  } finally {
    setProviderListPromise(null);
  }
};

/** Update an existing proxied-provider pairing. Busts the cache. */
export const updateProviderPairing = async (
  harnessId: string,
  providerId: string,
  baseUrl: string | null,
  modelTiers: ModelTiers | null,
): Promise<void> => {
  try {
    await _invoke<void>('update_provider_pairing', {
      harnessId,
      providerId,
      baseUrl,
      modelTiers,
    });
  } finally {
    setProviderListPromise(null);
  }
};

/** Detach a proxied-provider pairing. Busts the cache. */
export const removeProviderPairing = async (
  harnessId: string,
  providerId: string,
): Promise<void> => {
  try {
    await _invoke<void>('remove_provider_pairing', { harnessId, providerId });
  } finally {
    setProviderListPromise(null);
  }
};

// ----- Usage meters -----------------------------------------------------

export const getProviderMeters = (forceRefresh: boolean) =>
  _invoke<ProviderMeters[]>(
    'get_provider_meters',
    { forceRefresh },
  );

/** Telemetry event name (mirror of the Rust `MUSE_SESSION_TELEMETRY_EVENT`). */
export const MUSE_SESSION_TELEMETRY_EVENT = 'muse-session-telemetry';

export const getMuseSessionTelemetry = (nodeId: number) =>
  _invoke<
    ObservedMuseSessionTelemetry
  >('get_muse_session_telemetry', { nodeId });

// ----- Resolved harness view (issue #1656) ------------------------------

/**
 * Compute the per-harness cascade for one harness id (issue #1656).
 *
 * Returns the full breakdown (each layer's value + the capability-masked
 * resolved value for both `model` and `effort`) so the Settings modal +
 * spawn menu display the same cascade the spawn path enforces. The
 * backend command routes through the same
 * `preferences::resolver::cascade` helper the spawn path uses — the
 * cascade order lives in exactly one place.
 */
export const getResolvedHarnessView = (
  harnessId: string,
  meshId?: number | null,
) =>
  _invoke<
    ResolvedHarnessView
  >('get_resolved_harness_view', { harnessId, meshId });
