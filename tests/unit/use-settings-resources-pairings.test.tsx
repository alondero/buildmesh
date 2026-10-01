/**
 * Issue #1935 — the pairings load must not sit in `providers`' serial tail, and
 * the request-sequence guard (issue #1534 review round 5) must keep working
 * now that the load starts earlier and can be retried.
 *
 * These drive `useSettingsResources` directly rather than through the modal:
 * the hook owns the state machine (per-resource request ids, the boundary
 * check), so the contracts below are its own. The "a slow providers does not
 * delay pairings" claim is additionally pinned end-to-end in
 * `app-settings-resource-failure.test.tsx`.
 *
 * Ordering is driven with controlled promises resolved in the order the test
 * chooses — never with sleeps.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { renderHook, act, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import {
  useSettingsResources,
  type HostPairingVerificationsFn,
} from '../../src/components/AppSettings/useSettingsResources';
import type { AppPreferences, ProviderAccount, ProviderPairing } from '../../src/lib/tauri';
import { __resetProviderCachesForTests } from '../../src/lib/tauri';

/** A promise the test resolves by hand, so request A and B can land in any order. */
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

const PREFERENCES = {
  provider_pairings: [{ harness_id: 'claude', provider_id: 'minimax' }],
} as unknown as AppPreferences;

const DEEPSEEK: ProviderAccount = {
  id: 'deepseek',
  name: 'DeepSeek',
  enabled: true,
  billing_mode: 'pay_as_you_go',
  claude_compatible: true,
  api_key: 'sk-test',
};

const COMPATIBLE = { claude: [DEEPSEEK] } as unknown as Record<string, ProviderAccount[]>;

function pairing(providerId: string): ProviderPairing {
  return {
    harness_id: 'claude',
    provider_id: providerId,
    surface: 'anthropic',
    base_url: null,
    model_tiers: null,
  } as unknown as ProviderPairing;
}

/** Commands the pairings loader owns. `list_providers` is deliberately absent:
 * the loader must never reach for it. */
function mockPairingsIpc(overrides: Record<string, () => Promise<unknown>>) {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    const override = overrides[cmd];
    if (override) return override();
    switch (cmd) {
      case 'get_app_preferences':
        return Promise.resolve(PREFERENCES);
      case 'get_provider_pairings':
        return Promise.resolve([]);
      case 'get_pairing_verifications':
        return Promise.resolve([]);
      case 'compatible_providers_by_harness':
        return Promise.resolve(COMPATIBLE);
      default:
        return Promise.resolve({});
    }
  });
}

const noVerifications: HostPairingVerificationsFn = () => Promise.resolve([]);

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  __resetProviderCachesForTests();
});

describe('useSettingsResources — pairings load (#1935)', () => {
  it('loads the attach-picker map from one backend call and never reads the provider menu', async () => {
    const commands: string[] = [];
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      commands.push(cmd);
      switch (cmd) {
        case 'get_app_preferences':
          return Promise.resolve(PREFERENCES);
        case 'get_provider_pairings':
          return Promise.resolve([pairing('minimax')]);
        case 'get_pairing_verifications':
          return Promise.resolve([]);
        case 'compatible_providers_by_harness':
          return Promise.resolve(COMPATIBLE);
        default:
          return Promise.resolve({});
      }
    });

    const onPairingsLoaded = vi.fn();
    const { result } = renderHook(() =>
      useSettingsResources({ getHostPairingVerifications: noVerifications, onPairingsLoaded }),
    );

    let loaded: Awaited<ReturnType<typeof result.current.loadPairings>>;
    await act(async () => {
      loaded = await result.current.loadPairings();
    });

    expect(result.current.resources.pairings).toEqual({ status: 'loaded', error: null });
    // The whole map arrives in one call — not one call per harness.
    expect(commands.filter((c) => c === 'compatible_providers_by_harness')).toHaveLength(1);
    expect(commands).not.toContain('compatible_providers_for_harness');
    // The whole point of the issue: pairings never waits on — or asks for —
    // the provider menu.
    expect(commands).not.toContain('list_providers');
    expect(loaded?.effective).toHaveLength(1);
    expect(loaded?.compatible.claude?.[0]?.id).toBe('deepseek');
    // Stored keys still come from preferences, so the round-4 contract holds.
    expect(loaded?.storedKeys.has('claude:minimax')).toBe(true);
  });

  it('coalesces the preferences read shared with loadPreferences, but re-reads after it settles', async () => {
    const prefs = deferred<AppPreferences>();
    let prefsReads = 0;
    mockPairingsIpc({
      get_app_preferences: () => {
        prefsReads += 1;
        return prefs.promise;
      },
    });

    const { result } = renderHook(() =>
      useSettingsResources({ getHostPairingVerifications: noVerifications }),
    );

    // Both loaders start in the same tick, as they do on modal mount.
    let pairingsDone: Promise<unknown>;
    act(() => {
      void result.current.loadPreferences();
      pairingsDone = result.current.loadPairings();
    });

    // A second preferences read here would be the redundant round trip the
    // issue calls out — preferences is process-cached in the backend, but it
    // is still a round trip and a re-read of a store the modal already holds.
    expect(prefsReads).toBe(1);

    await act(async () => {
      prefs.resolve(PREFERENCES);
      await pairingsDone;
    });
    expect(result.current.resources.pairings.status).toBe('loaded');

    // The coalescing is per in-flight read, not a cache: a later load (a
    // post-mutation refresh) must see fresh state.
    await act(async () => {
      await result.current.loadPreferences();
    });
    expect(prefsReads).toBe(2);
  });

  it('discards a superseded pairings response instead of overwriting the newer one', async () => {
    const slow = deferred<ProviderPairing[]>();
    const fast = deferred<ProviderPairing[]>();
    let reads = 0;
    mockPairingsIpc({
      get_provider_pairings: () => {
        reads += 1;
        return reads === 1 ? slow.promise : fast.promise;
      },
    });

    const onPairingsLoaded = vi.fn();
    const { result } = renderHook(() =>
      useSettingsResources({ getHostPairingVerifications: noVerifications, onPairingsLoaded }),
    );

    type PairingsLoad = ReturnType<typeof result.current.loadPairings>;
    let firstRun!: PairingsLoad;
    let secondRun!: PairingsLoad;
    act(() => {
      firstRun = result.current.loadPairings();
      // A retry lands before the first request answers.
      secondRun = result.current.loadPairings();
    });

    let secondResult: Awaited<PairingsLoad>;
    await act(async () => {
      fast.resolve([pairing('kimi')]);
      secondResult = await secondRun;
    });
    expect(onPairingsLoaded).toHaveBeenCalledTimes(1);
    expect(onPairingsLoaded).toHaveBeenCalledWith(
      expect.objectContaining({ effective: [pairing('kimi')] }),
    );
    expect(secondResult?.effective).toEqual([pairing('kimi')]);

    // The stale first response arrives last and must be ignored — both by the
    // commit path and by the promise its caller is still holding.
    let firstResult: Awaited<PairingsLoad>;
    await act(async () => {
      slow.resolve([pairing('minimax')]);
      firstResult = await firstRun;
    });

    expect(onPairingsLoaded).toHaveBeenCalledTimes(1);
    expect(onPairingsLoaded).not.toHaveBeenCalledWith(
      expect.objectContaining({ effective: [pairing('minimax')] }),
    );
    expect(result.current.resources.pairings).toEqual({ status: 'loaded', error: null });
    expect(firstResult).toBeNull();
  });

  it('retries pairings unconditionally: the retry re-runs the read and recovers', async () => {
    // The trap this pins shut: `retryResource('pairings')` used to require the
    // caller to pass a non-empty providers list, and no call site ever did —
    // so Retry on a failed pairings load silently did nothing and replaced the
    // real error with "Awaiting providers list". A retry must now be just the
    // loader, with no precondition and no arguments.
    let fails = true;
    let reads = 0;
    mockPairingsIpc({
      get_provider_pairings: () => {
        reads += 1;
        return fails
          ? Promise.reject(new Error('pairings endpoint 500'))
          : Promise.resolve([pairing('minimax')]);
      },
    });

    const onPairingsLoaded = vi.fn();
    const { result } = renderHook(() =>
      useSettingsResources({ getHostPairingVerifications: noVerifications, onPairingsLoaded }),
    );

    await act(async () => {
      await result.current.loadPairings();
    });
    expect(result.current.resources.pairings).toEqual({
      status: 'failed',
      error: 'pairings endpoint 500',
    });
    expect(reads).toBe(1);

    // No providers list exists in this hook at all — the retry must not need one.
    fails = false;
    act(() => {
      result.current.retryResource('pairings');
    });

    await waitFor(() =>
      expect(result.current.resources.pairings).toEqual({ status: 'loaded', error: null }),
    );
    // The read really was re-issued, and the recovered state carries the data.
    expect(reads).toBe(2);
    expect(onPairingsLoaded).toHaveBeenCalledWith(
      expect.objectContaining({ effective: [pairing('minimax')] }),
    );
  });

  it('keeps the preferences read best-effort for pairings: a preferences failure does not fail the resource', async () => {
    const onPairingsLoaded = vi.fn();
    mockPairingsIpc({
      get_app_preferences: () => Promise.reject(new Error('preferences.json is corrupt')),
    });
    const { result } = renderHook(() =>
      useSettingsResources({ getHostPairingVerifications: noVerifications, onPairingsLoaded }),
    );

    await act(async () => {
      await result.current.loadPairings();
    });

    expect(result.current.resources.pairings).toEqual({ status: 'loaded', error: null });
    expect(onPairingsLoaded).toHaveBeenCalledWith(
      expect.objectContaining({
        storedKeys: new Set<string>(),
        compatible: COMPATIBLE,
      }),
    );
  });
});
