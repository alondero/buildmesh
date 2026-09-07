/**
 * Issue #1534 — Settings modal must isolate resource-load failures so a
 * single transient IPC error doesn't enable the rest of the modal with
 * placeholder defaults. Each test below configures ONE tauri command to
 * reject and asserts the observable consequences:
 *
 *   - controls that depend on the failed resource stay disabled,
 *   - siblings that loaded successfully still render their real values,
 *   - the failed resource shows an accessible error + Retry,
 *   - collections (accounts, devices, pairings) don't silently claim to
 *     be empty when the real answer is "we don't know".
 *
 * The tests mock `invoke` per-command rather than stubbing internal
 * state, so they pin the public contract: the modal reacts to a
 * rejection from a specific command the way the issue requires.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import { openSettingsPane } from '../utils/settings-panes';
import { __resetProviderCachesForTests } from '../../src/lib/tauri';
import type { ProviderInfo } from '../../src/lib/tauri';

const NO_OVERRIDE_VALUE = '__no_override__';

function provider(id: string, label: string): ProviderInfo {
  return {
    id,
    label,
    color: '#fff',
    icon: id,
    resumable: false,
    harness_id: id,
    provider_id: null,
    is_proxied: false,
    group_key: id,
    capabilities: {
      harness_id: id,
      supports_resume: false,
      auto_resume_on_startup: false,
      requires_attention_hook: false,
      produces_readable_transcript: false,
      // Issue #1534 regression — the harness-defaults section needs a
      // `supports_model_override: true` harness so its `disabled` prop
      // (wired from `!prefsLoaded`) actually has a model input to
      // disable under a failed preferences load.
      supports_model_override: id !== 'terminal',
      supports_effort_override: false,
      supports_prefill: false,
      is_plain_terminal: id === 'terminal',
      effort_control: { kind: 'none' },
      available_on: ['windows'],
    },
  };
}

const REAL_PROVIDERS: ProviderInfo[] = [
  provider('anthropic', 'Anthropic'),
  provider('codex', 'Codex'),
  provider('terminal', 'Terminal'),
];

const REAL_ACCOUNTS = [
  { id: 'anthropic', name: 'Anthropic / Claude', enabled: true, billing_mode: 'plan' as const, claude_compatible: false, api_key: null },
];

const REAL_DEVICES = [
  { id: 7, label: "Adam's iPhone", last_ip: '10.0.0.42', last_active_at: 'just now' },
];

const REAL_NETWORK = {
  lan_exposure_enabled: true,
  port: 1992,
  tls_active: true,
  exposed_interfaces: [{ address: '10.0.0.5:1992', tls: true }],
};

/** Build a per-command mock where every command resolves successfully
 *  EXCEPT for the named command, which rejects with `errorMessage`.
 *  Anything not enumerated falls through to `{}` (mirrors the
 *  vitest.setup.ts default). */
function mockWithFailure(failingCmd: string, errorMessage: string) {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === failingCmd) {
      return Promise.reject(new Error(errorMessage));
    }
    switch (cmd) {
      case 'get_app_preferences':
        return Promise.resolve({
          default_provider: 'anthropic',
          minimax_api_key: null,
          naming_provider: null,
          autopilot_pool_size: 5,
          worktree_directory: '',
          confirm_before_quit: true,
          harness_defaults: {},
          provider_pairings: [],
        });
      case 'list_providers':
        return Promise.resolve(REAL_PROVIDERS);
      case 'get_provider_accounts':
        return Promise.resolve(REAL_ACCOUNTS);
      case 'get_keyed_first_class_catalog':
        return Promise.resolve([]);
      case 'get_provider_pairings':
        return Promise.resolve([]);
      case 'get_pairing_verifications':
        return Promise.resolve([]);
      case 'compatible_providers_for_harness':
        return Promise.resolve([]);
      case 'get_coordinator_status':
        return Promise.resolve({ enabled: false, has_token: false });
      case 'list_device_sessions':
        return Promise.resolve(REAL_DEVICES);
      case 'get_network_status':
        return Promise.resolve(REAL_NETWORK);
      default:
        return Promise.resolve({});
    }
  });
}

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  __resetProviderCachesForTests();
});

describe('AppSettingsModal — resource-load failure isolation (#1534)', () => {
  it('a network-status rejection keeps the LAN toggle disabled with Retry, but the rest of the modal renders real values', async () => {
    mockWithFailure('get_network_status', 'network status offline');

    render(<AppSettingsModal onClose={() => {}} />);

    // Wait for the modal to settle: the default provider dropdown
    // reads from `preferences`, which loaded successfully — the
    // selected option text must reflect the real `'anthropic'` value
    // rather than the `NO_OVERRIDE` placeholder default.
    const defaultProvider = await screen.findByLabelText<HTMLSelectElement>('Default provider');
    await waitFor(() => {
      expect(defaultProvider.value).toBe('anthropic');
    });

    // Switch to Remote Access pane; the network resource has failed
    // so the LAN toggle must be disabled AND an error banner with
    // Retry must be visible.
    await openSettingsPane(/^Remote Access$/);
    const lanCheckbox = await screen.findByLabelText(/expose to lan/i);
    expect(lanCheckbox.hasAttribute('disabled')).toBe(true);
    const networkBanner = await screen.findByTestId('resource-load-network');
    expect(networkBanner).toBeTruthy();
    expect(networkBanner.textContent).toMatch(/network status/i);

    // The error message must surface verbatim so a transient IPC
    // rejection can be correlated with a backend log entry.
    expect(networkBanner.textContent).toContain('network status offline');

    // The Retry button must re-fire ONLY the failed resource. The
    // other panes' real data must stay where it was (we assert by
    // re-checking the default-provider value).
    const retryBtn = await screen.findByTestId('resource-load-network-retry');
    expect(retryBtn).toBeTruthy();
    // Re-arm the mock so the retry resolves successfully this time.
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_network_status') return Promise.resolve(REAL_NETWORK);
      return Promise.resolve({});
    });
    fireEvent.click(retryBtn);
    await waitFor(() => {
      // Once the retry succeeds, the banner disappears; the LAN
      // checkbox is now enabled.
      expect(screen.queryByTestId('resource-load-network')).toBeNull();
      expect(screen.queryByTestId('resource-load-network-retry')).toBeNull();
    });
    expect(lanCheckbox.hasAttribute('disabled')).toBe(false);
  });

  it('a preferences rejection disables every preference-backed setter while leaving the providers/accounts/devices lists usable', async () => {
    mockWithFailure('get_app_preferences', 'preferences.json corrupt');

    render(<AppSettingsModal onClose={() => {}} />);

    // The default provider select must render but its value stays at
    // the placeholder `NO_OVERRIDE` — a failed preferences load means
    // we genuinely don't know the persisted value, so the control is
    // disabled rather than showing a fabricated default.
    const defaultProvider = await screen.findByLabelText<HTMLSelectElement>('Default provider');
    await waitFor(() => {
      expect(defaultProvider.hasAttribute('disabled')).toBe(true);
    });
    expect(defaultProvider.value).toBe(NO_OVERRIDE_VALUE);

    // The other preference-backed controls also disable.
    expect(screen.getByLabelText('Autopilot pool size').hasAttribute('disabled')).toBe(true);
    expect(screen.getByLabelText('Worktree directory').hasAttribute('disabled')).toBe(true);
    expect(
      screen.getByRole('checkbox', { name: /confirm before quitting/i }).hasAttribute('disabled'),
    ).toBe(true);

    // The Harness Defaults section is also preference-backed. Its
    // `disabled` prop must propagate to the per-harness model input
    // + effort select so a failed preferences load can't write empty
    // defaults as if they were the persisted value (issue #1534).
    // Use `findBy` because the section hydrates from the (failed)
    // preferences read. `harness_id` defaults to the provider id
    // in our test fixture, so `Anthropic` renders the
    // `harness-default-model-input-anthropic` testid.
    const harnessModel = await screen.findByTestId('harness-default-model-input-anthropic');
    expect(harnessModel.hasAttribute('disabled')).toBe(true);

    // The error banner for preferences is visible at the top of the
    // General pane.
    const prefsBanner = await screen.findByTestId('resource-load-preferences');
    expect(prefsBanner.textContent).toContain('preferences.json corrupt');

    // Switch to the Providers pane — `get_provider_accounts`
    // succeeded independently, so its account list is fully usable.
    await openSettingsPane('Providers');
    expect(await screen.findByText('Anthropic / Claude')).toBeTruthy();

    // Switch to Remote Access — devices and network both succeeded
    // independently, so the LAN toggle enables and the device list
    // renders the real "Adam's iPhone" row.
    await openSettingsPane(/^Remote Access$/);
    expect(await screen.findByText("Adam's iPhone")).toBeTruthy();
    expect(screen.getByLabelText(/expose to lan/i).hasAttribute('disabled')).toBe(false);
  });

  it('a devices rejection surfaces an explicit error instead of "No paired devices yet"', async () => {
    mockWithFailure('list_device_sessions', 'device list unavailable');

    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/^Remote Access$/);

    // The failure-isolation contract: the pane must NOT claim there
    // are zero authorized devices when the real answer is "we don't
    // know". The placeholder string would be a worse lie than the
    // empty list, because the user would assume the device was
    // revoked or never paired.
    expect(screen.queryByText(/no paired devices yet/i)).toBeNull();
    expect(await screen.findByTestId('resource-load-devices')).toBeTruthy();

    // The error must surface the underlying message verbatim.
    const banner = screen.getByTestId('resource-load-devices');
    expect(banner.textContent).toContain('device list unavailable');

    // Retry button is present so a transient failure can be
    // recovered without reopening the modal.
    expect(screen.getByTestId('resource-load-devices-retry')).toBeTruthy();
  });

  it('a pairings rejection surfaces an explicit error in the Harnesses pane instead of "no compatible providers"', async () => {
    mockWithFailure('get_provider_pairings', 'pairings endpoint 500');

    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane('Harnesses');

    // The Harnesses pane used to silently swallow a pairings failure
    // (console.error + empty state). Now the failure is surfaced as
    // an explicit banner so the user can retry — an empty
    // "compatible providers" picker would be indistinguishable from
    // a genuinely empty catalog.
    const banner = await screen.findByTestId('resource-load-pairings');
    expect(banner.textContent).toContain('pairings endpoint 500');

    // The Retry button is wired to `retryResource('pairings')`.
    expect(screen.getByTestId('resource-load-pairings-retry')).toBeTruthy();
  });

  it('an accounts rejection renders the error banner above the providers pane and disables account cards', async () => {
    mockWithFailure('get_provider_accounts', 'accounts fetch failed');

    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane('Providers');

    const banner = await screen.findByTestId('resource-load-accounts');
    expect(banner.textContent).toContain('accounts fetch failed');

    // The account list must not pretend the user has zero accounts.
    expect(screen.queryByText('Anthropic / Claude')).toBeNull();
  });

  it('retrying one resource rehydrates only that resource, leaving siblings alone', async () => {
    // Both network and preferences fail on first attempt; the test
    // then retries network only and asserts preferences is still
    // showing its banner (not silently flipped to "loaded").
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_network_status') return Promise.reject(new Error('network down'));
      if (cmd === 'get_app_preferences') return Promise.reject(new Error('prefs down'));
      switch (cmd) {
        case 'list_providers':
          return Promise.resolve(REAL_PROVIDERS);
        case 'get_provider_accounts':
          return Promise.resolve(REAL_ACCOUNTS);
        case 'get_keyed_first_class_catalog':
          return Promise.resolve([]);
        case 'get_provider_pairings':
          return Promise.resolve([]);
        case 'get_pairing_verifications':
          return Promise.resolve([]);
        case 'compatible_providers_for_harness':
          return Promise.resolve([]);
        case 'get_coordinator_status':
          return Promise.resolve({ enabled: false, has_token: false });
        case 'list_device_sessions':
          return Promise.resolve(REAL_DEVICES);
        default:
          return Promise.resolve({});
      }
    });

    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/^Remote Access$/);

    // Both banners present.
    expect(await screen.findByTestId('resource-load-network')).toBeTruthy();
    // The preferences banner lives on the General pane; switch to
    // confirm it stayed in place across retries.
    await openSettingsPane(/^General$/);
    const prefsBanner = await screen.findByTestId('resource-load-preferences');
    expect(prefsBanner.textContent).toContain('prefs down');

    // Re-arm the network mock and click its retry button.
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      if (cmd === 'get_network_status') return Promise.resolve(REAL_NETWORK);
      if (cmd === 'get_app_preferences') return Promise.reject(new Error('prefs down'));
      return Promise.resolve({});
    });
    await openSettingsPane(/^Remote Access$/);
    fireEvent.click(await screen.findByTestId('resource-load-network-retry'));
    await waitFor(() => {
      expect(screen.queryByTestId('resource-load-network')).toBeNull();
    });

    // Network banner gone, preferences banner still present (its
    // sibling resource was NOT touched by the retry).
    expect(screen.queryByTestId('resource-load-network')).toBeNull();
    await openSettingsPane(/^General$/);
    expect(screen.getByTestId('resource-load-preferences')).toBeTruthy();
  });
});