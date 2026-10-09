/**
 * Issue #2154 — the accounts loader's credential-store probe.
 *
 * The Settings notice says "this key is in preferences.json" for the affected
 * account. The probe that decides which accounts those are is auxiliary: it
 * must never take the account cards down with it. The two contracts that are
 * easy to get wrong, and are pinned here:
 *
 *   1. A rejected probe does NOT fail the `accounts` resource. The cards must
 *      still render; an IPC error is not a reason to blank the Accounts pane.
 *   2. A rejected probe is `null` ("unknown"), never `[]` ("no keys on disk").
 *      Collapsing the two would let the app assert the one thing this notice
 *      exists to stop it from assuming: that a key is safely stored when in
 *      fact nobody checked.
 *
 * Driven through the real hook, so the state machine and the resource status
 * are the hook's own rather than a re-implementation.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { useSettingsResources } from '../../src/components/AppSettings/useSettingsResources';
import type { ProviderAccount } from '../../src/lib/tauri';
import { __resetProviderCachesForTests } from '../../src/lib/tauri';

const MINIMAX: ProviderAccount = {
  id: 'minimax',
  name: 'MiniMax',
  enabled: true,
  billing_mode: 'pay_as_you_go',
  claude_compatible: true,
  api_key: 'sk-live-0001',
};

function mockAccountsIpc(probe: () => Promise<unknown>) {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    switch (cmd) {
      case 'get_provider_accounts':
        return Promise.resolve([MINIMAX]);
      case 'get_keyed_first_class_catalog':
        return Promise.resolve([]);
      case 'get_provider_accounts_with_preferences_keys':
        return probe();
      default:
        return Promise.resolve({});
    }
  });
}

/** Load the accounts resource and hand back both halves of the outcome. */
async function loadAccounts() {
  const onAccountsLoaded = vi.fn();
  const { result } = renderHook(() => useSettingsResources({ onAccountsLoaded }));
  await act_load(result.current.loadAccounts());
  return { status: result.current.resources.accounts.status, onAccountsLoaded };
}

/** `act` without importing it twice per call site. */
async function act_load<T>(p: Promise<T>): Promise<T> {
  const { act } = await import('@testing-library/react');
  let out!: T;
  await act(async () => {
    out = await p;
  });
  return out;
}

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  __resetProviderCachesForTests();
  vi.spyOn(console, 'warn').mockImplementation(() => {});
});

describe('useSettingsResources — credential-store probe (#2154)', () => {
  it('reports the accounts whose keys are on disk', async () => {
    mockAccountsIpc(() => Promise.resolve(['minimax']));

    const { status, onAccountsLoaded } = await loadAccounts();

    expect(status).toBe('loaded');
    expect(onAccountsLoaded).toHaveBeenCalledWith(
      expect.objectContaining({ keysInPreferences: ['minimax'] }),
    );
  });

  it('reports an empty list when every key is in the credential store', async () => {
    mockAccountsIpc(() => Promise.resolve([]));

    const { status, onAccountsLoaded } = await loadAccounts();

    expect(status).toBe('loaded');
    expect(onAccountsLoaded).toHaveBeenCalledWith(
      expect.objectContaining({ keysInPreferences: [] }),
    );
  });

  it('still loads the accounts when the probe rejects, reporting unknown', async () => {
    mockAccountsIpc(() => Promise.reject(new Error('credential store probe failed')));

    const { status, onAccountsLoaded } = await loadAccounts();

    // The cards must render: an auxiliary probe failing is not a reason to
    // blank the Accounts pane behind an error banner.
    expect(status).toBe('loaded');
    expect(onAccountsLoaded).toHaveBeenCalledWith(
      expect.objectContaining({
        accountList: [MINIMAX],
        // `null`, not `[]`. `[]` would tell the UI every key is stored.
        keysInPreferences: null,
      }),
    );
  });

  it('treats a malformed probe payload as unknown rather than empty', async () => {
    mockAccountsIpc(() => Promise.resolve({ oops: true }));

    const { status, onAccountsLoaded } = await loadAccounts();

    expect(status).toBe('loaded');
    await waitFor(() =>
      expect(onAccountsLoaded).toHaveBeenCalledWith(
        expect.objectContaining({ keysInPreferences: null }),
      ),
    );
  });
});