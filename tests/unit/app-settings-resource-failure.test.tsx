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

    // The preferences error banner is rendered in each pane that has
    // preference-backed controls (General, Providers, Harnesses), so several
    // copies exist in the mounted tree — assert on the first.
    const prefsBanners = await screen.findAllByTestId('resource-load-preferences');
    expect(prefsBanners[0].textContent).toContain('preferences.json corrupt');

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
    const prefsBanners = await screen.findAllByTestId('resource-load-preferences');
    expect(prefsBanners[0].textContent).toContain('prefs down');

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
    expect(screen.getAllByTestId('resource-load-preferences').length).toBeGreaterThan(0);
  });

  it('a list_providers rejection renders a providers banner on the Providers pane (round-2 review)', async () => {
    // Issue #1534 review round 2 — without this banner the user sees the
    // default-provider + auto-naming controls silently disabled with no
    // explanation. Those routing controls now live on the Providers pane, so
    // the banner must surface there.
    mockWithFailure('list_providers', 'providers endpoint 503');

    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane('Providers');

    // The providers banner also appears on the hidden Harnesses pane (mounted
    // but not visible), so we use `getAllByTestId` and check at least one copy
    // exists with the right text.
    const banners = await screen.findAllByTestId('resource-load-providers');
    expect(banners.length).toBeGreaterThanOrEqual(1);
    expect(banners[0].textContent).toContain('providers endpoint 503');
    const retryButtons = screen.getAllByTestId('resource-load-providers-retry');
    expect(retryButtons[0].getAttribute('aria-label')).toMatch(/retry loading providers/i);

    // Default provider select is disabled (it depends on providers
    // AND preferences; here providers is failed so the dropdown is
    // not selectable).
    const defaultProvider = screen.getByLabelText<HTMLSelectElement>('Default provider');
    expect(defaultProvider.hasAttribute('disabled')).toBe(true);
  });

  it('a get_coordinator_status rejection renders a coordinator banner on the Remote pane (round-2 review)', async () => {
    // Round-2 review — coordinator failures used to silently
    // disable the coordinator toggle on the Remote pane with no
    // visible explanation. The toggle is still disabled; the banner
    // now makes the failure visible.
    mockWithFailure('get_coordinator_status', 'coordinator endpoint down');

    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/^Remote Access$/);

    const banner = await screen.findByTestId('resource-load-coordinator');
    expect(banner.textContent).toContain('coordinator endpoint down');

    // The coordinator toggle is disabled AND the banner is visible
    // so the user can recover via Retry (not "the toggle is off
    // and I can't tell why").
    const coordToggle = screen.getByRole('checkbox', { name: /enable coordinator read api/i });
    expect(coordToggle.hasAttribute('disabled')).toBe(true);
  });

  it('switching to the Harnesses pane while pairings is still idle shows the loading state, not an empty HarnessConfigList (round-2 review)', async () => {
    // Round-2 review showstopper — pairings starts at `idle` until
    // providers loads. Navigating to the Harnesses pane mid-load
    // used to fall through the ternary's else branch and mount an
    // empty HarnessConfigList, flashing "no compatible providers"
    // — the exact bug #1534 aimed to fix.
    //
    // We mock listProviders to hang (never resolves) so the
    // chain never gets to pairings. Switching to Harnesses must
    // therefore render the loading banner, NOT HarnessConfigList.
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'get_app_preferences':
          return Promise.resolve({
            default_provider: null,
            naming_provider: null,
            autopilot_pool_size: null,
            worktree_directory: '',
            confirm_before_quit: true,
            harness_defaults: {},
            provider_pairings: [],
          });
        case 'list_providers':
          // Never resolve — providers stays in `loading` indefinitely,
          // so pairings stays in `idle`.
          return new Promise(() => {});
        case 'get_provider_accounts':
          return Promise.resolve([]);
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
          return Promise.resolve([]);
        case 'get_network_status':
          return Promise.resolve(REAL_NETWORK);
        default:
          return Promise.resolve({});
      }
    });

    render(<AppSettingsModal onClose={() => {}} />);

    // Switch to Harnesses before any load resolves. The testid for
    // the loading state is `resource-load-pairings-loading`; the
    // empty-state text is "Spawn menu order" — neither should be
    // present because HarnessConfigList must not mount.
    await openSettingsPane('Harnesses');
    expect(await screen.findByTestId('resource-load-pairings-loading')).toBeTruthy();
    // The "Spawn menu order" section + HarnessConfigList are gated
    // on at least two orderable harnesses; with `providers` empty
    // the section is hidden anyway, but the loading banner is what
    // we're pinning — it's the visible signal that the data isn't
    // ready.
    expect(screen.queryByText(/no compatible providers/i)).toBeNull();
  });

  it('retrying pairings while providers is empty marks pairings failed (no fabricated clean state)', async () => {
    // Round-2 review — the previous `retryResource('pairings')`
    // called `loadPairings(providersRef.current)` which was `[]`
    // when providers had failed. `loadPairings([])` then queried
    // zero harnesses, "succeeded" with empty arrays, and the
    // error banner disappeared — lying to the user.
    //
    // Now `loadPairings([])` short-circuits to a `failed` state
    // with an explicit message. The user must retry providers
    // before pairings can load.
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'get_app_preferences':
          return Promise.resolve({
            default_provider: null,
            naming_provider: null,
            autopilot_pool_size: null,
            worktree_directory: '',
            confirm_before_quit: true,
            harness_defaults: {},
            provider_pairings: [],
          });
        case 'list_providers':
          return Promise.reject(new Error('providers offline'));
        case 'get_provider_accounts':
          return Promise.resolve([]);
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
          return Promise.resolve([]);
        case 'get_network_status':
          return Promise.resolve(REAL_NETWORK);
        default:
          return Promise.resolve({});
      }
    });

    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane('Harnesses');

    // With providers failed, the Harnesses pane must show the
    // providers banner (NOT pairings — pairings never even tried).
    const providerBanners = await screen.findAllByTestId('resource-load-providers');
    expect(providerBanners.length).toBeGreaterThanOrEqual(1);
    expect(providerBanners[0].textContent).toContain('providers offline');

    // Pairings must NOT be in the `loaded` state — without providers,
    // pairings has no harnesses to query, and the previous code
    // would have "succeeded" with zero harnesses + zero compatible
    // providers, lying to the user. Now it stays at `idle` and we
    // see no "No compatible providers" copy.
    expect(screen.queryByText(/no compatible providers/i)).toBeNull();
  });

  it('pairings load decoupled from preferences: a corrupted preferences.json does NOT fail the pairings resource', async () => {
    // Round-2 review — the previous `loadPairings` called
    // `getAppPreferences()` for `provider_pairings` (the stored-key
    // set). A corrupted preferences.json would crash pairings even
    // when pairings + verifications + compatibility were all fine.
    //
    // After the fix: preferences failure inside pairings is
    // best-effort; pairings loads the core data and falls back to
    // an empty stored-key set. The pairings resource reaches
    // `loaded`, NOT `failed`.
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'get_app_preferences':
          // Reject — a corrupted preferences.json.
          return Promise.reject(new Error('preferences.json corrupt'));
        case 'list_providers':
          return Promise.resolve(REAL_PROVIDERS);
        case 'get_provider_accounts':
          return Promise.resolve([]);
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
          return Promise.resolve([]);
        case 'get_network_status':
          return Promise.resolve(REAL_NETWORK);
        default:
          return Promise.resolve({});
      }
    });

    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane('Harnesses');

    // Pairings must reach `loaded` (no banner, no loading) even
    // though preferences failed. The HarnessConfigList is now
    // rendered with the best-effort empty stored-key set; rows
    // show as detachable, which is the documented fallback.
    await waitFor(() => {
      expect(screen.queryByTestId('resource-load-pairings')).toBeNull();
      expect(screen.queryByTestId('resource-load-pairings-loading')).toBeNull();
    });

    // Meanwhile the preferences banner is on the General pane.
    await openSettingsPane(/^General$/);
    expect(screen.getAllByTestId('resource-load-preferences').length).toBeGreaterThan(0);
  });

  it('a list_accounts rejection is isolated from the keyed catalog (round-4 review)', async () => {
    // Issue #1534 (review round 4) — bundling
    // `getKeyedFirstClassCatalog()` with `getProviderAccounts()` in a
    // single `Promise.all` meant a transient catalog rejection hid
    // the user's real accounts behind a generic failure banner.
    // After the fix, the catalog lookup is best-effort — accounts
    // still load successfully even when catalog returns a
    // rejection.
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'get_app_preferences':
          return Promise.resolve({
            default_provider: null,
            naming_provider: null,
            autopilot_pool_size: null,
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
          // Catalog fails — must not take down accounts.
          return Promise.reject(new Error('catalog 503'));
        case 'get_provider_pairings':
          return Promise.resolve([]);
        case 'get_pairing_verifications':
          return Promise.resolve([]);
        case 'compatible_providers_for_harness':
          return Promise.resolve([]);
        case 'get_coordinator_status':
          return Promise.resolve({ enabled: false, has_token: false });
        case 'list_device_sessions':
          return Promise.resolve([]);
        case 'get_network_status':
          return Promise.resolve(REAL_NETWORK);
        default:
          return Promise.resolve({});
      }
    });

    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane('Providers');

    // Accounts card list renders (Anthropic / Claude).
    expect(await screen.findByText('Anthropic / Claude')).toBeTruthy();
    // No failure banner — accounts is `loaded`, not `failed`.
    expect(screen.queryByTestId('resource-load-accounts')).toBeNull();
  });

  it('an empty providers list is not an error for pairings (round-4 review)', async () => {
    // Issue #1534 (review round 4) — the old round-2 guard inside
    // `loadPairings` permanently locked pairings into `failed`
    // when the user had zero non-terminal harnesses. The fix
    // removed the guard from the core loader (an empty result set
    // IS the correct answer in that environment). This test pins
    // that contract: a providers list with only the Terminal
    // placeholder yields `pairings.status === 'loaded'`, not
    // `failed`, so the Harnesses pane renders HarnessConfigList
    // (with empty rows) instead of an error banner.
    vi.mocked(invoke).mockImplementation((cmd: string) => {
      switch (cmd) {
        case 'get_app_preferences':
          return Promise.resolve({
            default_provider: null,
            naming_provider: null,
            autopilot_pool_size: null,
            worktree_directory: '',
            confirm_before_quit: true,
            harness_defaults: {},
            provider_pairings: [],
          });
        case 'list_providers':
          // Only the Terminal placeholder — zero non-terminal
          // harnesses. pairings has nothing to query.
          return Promise.resolve([
            provider('terminal', 'Terminal'),
          ]);
        case 'get_provider_accounts':
          return Promise.resolve([]);
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
          return Promise.resolve([]);
        case 'get_network_status':
          return Promise.resolve(REAL_NETWORK);
        default:
          return Promise.resolve({});
      }
    });

    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane('Harnesses');

    // Pairings must reach `loaded` (NOT `failed`). The
    // "awaiting providers" message from the old guard must NOT
    // appear — empty input is no longer treated as a failure.
    await waitFor(() => {
      expect(screen.queryByTestId('resource-load-pairings')).toBeNull();
      expect(screen.queryByTestId('resource-load-pairings-loading')).toBeNull();
    });
    expect(screen.queryByText(/awaiting providers/i)).toBeNull();
  });

  it('handleAttachProvider awaits the loadPairings chain after loadProviders settles (round-5 review, source-shape assertion)', async () => {
    // Issue #1534 (review round 5) — the round-4 count-equality
    // test was a paper-tiger. The structural fix (await on the
    // chain) moved first to the modal, then to the
    // `useSettingsResources` hook in round 5. We pin both contracts
    // structurally here:
    //
    //   1. The hook's initial-mount fan-out does NOT chain
    //      `loadPairings` into `loadProviders` (round 5 removed
    //      that auto-chain so account-only refreshes don't probe
    //      WSL harness verifications).
    //   2. The hook's retryResource handles the pairings
    //      dependency guard: pairings retry without providers
    //      yields a "Awaiting providers" failure message.
    //
    // A future regression on either would re-introduce either the
    // cascade-probe IPC cost or the "load pairings with empty
    // providers fabricates success" bug. Both are guarded here.
    const { readFileSync } = await import('node:fs');
    const { resolve } = await import('node:path');
    const hookPath = resolve(process.cwd(), 'src/components/AppSettings/useSettingsResources.ts');
    const hookSrc = readFileSync(hookPath, 'utf8');

    // 1. `loadProviders` does NOT auto-chain `loadPairings`. (The
    //    auto-chain used to be inside `loadProviders`'s body;
    //    round 5 moved it to the harness-mutation handlers in the
    //    modal. The hook itself only loads the providers list.)
    expect(hookSrc).toMatch(/const loadProviders = useCallback/);
    const loadProvidersBody = hookSrc.match(
      /const loadProviders = useCallback[\s\S]+?\}\s*,\s*\[[^\]]+\]\);/,
    )?.[0] ?? '';
    expect(loadProvidersBody).not.toMatch(/loadPairings\(/);

    // 2. `retryResource`'s pairings branch checks for providers
    //    and surfaces an "Awaiting providers" message — never
    //    fabricates success with an empty providers list.
    const retryBody = hookSrc.match(
      /const retryResource = useCallback[\s\S]+?\n  \);/,
    )?.[0] ?? '';
    expect(retryBody).toMatch(/'pairings'/);
    expect(retryBody).toMatch(/Awaiting providers list/);
  });
});