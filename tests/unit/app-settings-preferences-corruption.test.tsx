/**
 * Issue #1523 — the Settings modal must render an *actionable* corruption
 * state for `preferences.json`, and must not offer to write settings over a
 * file the backend cannot read.
 *
 * The backend contract these tests pin (the Rust side is covered by
 * `preferences::tests::recovery_tests`):
 *
 *   - `get_app_preferences` still resolves — with defaults — so read-only
 *     surfaces keep working,
 *   - `get_preferences_health` is a *successful* call that reports whether
 *     those values are real settings or stand-ins,
 *   - `restore_preferences_backup` / `reset_app_preferences` are the only
 *     ways out, and every settings control is disabled until one is used.
 *
 * `invoke` is mocked per command (not internal state stubbed) so the tests
 * assert the public IPC contract, matching
 * `app-settings-resource-failure.test.tsx`.
 */
import { describe, it, expect, beforeEach, vi } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import { __resetProviderCachesForTests } from '../../src/lib/tauri';
import { useToastStore } from '../../src/stores/toastStore';
import type { CorruptionInfo } from '../../src/lib/tauri';

const CORRUPT_PATH = 'C:\\Users\\test\\AppData\\Roaming\\com.alond.buildmesh\\preferences.json';
const BACKUP_PATH =
  'C:\\Users\\test\\AppData\\Roaming\\com.alond.buildmesh\\preferences.json.bak';

function corruption(overrides: Partial<CorruptionInfo> = {}): CorruptionInfo {
  return {
    reason: 'invalid_json',
    detail: 'the file is not valid JSON (malformed JSON at line 1 column 24)',
    path: CORRUPT_PATH,
    byte_len: 812,
    backup_available: true,
    backup_path: BACKUP_PATH,
    ...overrides,
  };
}

const REAL_PROVIDERS = [
  {
    id: 'anthropic',
    label: 'Anthropic',
    color: '#fff',
    icon: 'anthropic',
    resumable: false,
    harness_id: 'anthropic',
    provider_id: null,
    is_proxied: false,
    group_key: 'anthropic',
    capabilities: {
      harness_id: 'anthropic',
      supports_resume: false,
      auto_resume_on_startup: false,
      requires_attention_hook: false,
      produces_readable_transcript: false,
      supports_model_override: true,
      supports_effort_override: false,
      supports_prefill: false,
      is_plain_terminal: false,
      effort_control: { kind: 'none' },
      available_on: ['windows'],
    },
  },
  // A second orderable harness: `HarnessOrderList` renders nothing at all
  // with fewer than two rows, so the reorder-lock assertion needs both.
  {
    id: 'codex',
    label: 'Codex',
    color: '#000',
    icon: 'codex',
    resumable: true,
    harness_id: 'codex',
    provider_id: null,
    is_proxied: false,
    group_key: 'codex',
    capabilities: {
      harness_id: 'codex',
      supports_resume: true,
      auto_resume_on_startup: true,
      requires_attention_hook: false,
      produces_readable_transcript: true,
      supports_model_override: true,
      supports_effort_override: true,
      supports_prefill: false,
      is_plain_terminal: false,
      effort_control: { kind: 'none' },
      available_on: ['windows'],
    },
  },
];

/** Per-command mock. `health` is the only variable the tests change, so
 *  everything else resolves exactly as it would in a healthy app. */
function mockApp({
  health,
  overrides = () => undefined,
}: {
  health: unknown;
  overrides?: (cmd: string) => unknown | undefined;
}) {
  vi.mocked(invoke).mockImplementation((cmd: string, args?: unknown) => {
    const override = overrides(cmd);
    if (override !== undefined) return Promise.resolve(override);
    switch (cmd) {
      case 'get_app_preferences':
        return Promise.resolve({
          default_provider: null,
          reviewer_provider: null,
          naming_provider: null,
          circuit_agent_pool_size: null,
          worktree_directory: '',
          confirm_before_quit: true,
          harness_defaults: {},
          // The *stored* list: it is what marks a pairing row user-detachable
          // (and therefore what makes its Detach / Edit controls exist).
          provider_pairings: [
            {
              harness_id: 'anthropic',
              provider_id: 'minimax',
              surface: 'anthropic',
              base_url: 'https://api.minimax.example',
              model_tiers: {},
            },
          ],
          provider_accounts: [
            {
              id: 'minimax',
              name: 'MiniMax',
              enabled: true,
              billing_mode: 'pay_as_you_go',
              claude_compatible: true,
              api_key: 'sk-test',
              base_url: 'https://api.minimax.example',
            },
          ],
          issue_spawn_prompt: null,
          pr_spawn_prompt: null,
          spawn_configurations: [],
        });
      case 'get_preferences_health':
        return Promise.resolve(health);
      case 'list_providers':
        return Promise.resolve(REAL_PROVIDERS);
      case 'get_provider_accounts':
        // One stored account so the Accounts tab renders a real card — the
        // gating assertion needs its controls to exist.
        return Promise.resolve([
          {
            id: 'minimax',
            name: 'MiniMax',
            enabled: true,
            billing_mode: 'pay_as_you_go',
            claude_compatible: true,
            api_key: 'sk-test',
            base_url: 'https://api.minimax.example',
          },
        ]);
      case 'get_keyed_first_class_catalog':
        return Promise.resolve([]);
      case 'get_provider_pairings':
        // One stored pairing so the Harnesses tab renders a detachable row.
        return Promise.resolve([
          {
            harness_id: 'anthropic',
            provider_id: 'minimax',
            surface: 'anthropic',
            base_url: 'https://api.minimax.example',
            model_tiers: {},
          },
        ]);
      case 'get_pairing_verifications':
        return Promise.resolve([]);
      case 'compatible_providers_by_harness':
        return Promise.resolve({});
      case 'get_coordinator_status':
        return Promise.resolve({ enabled: false, has_token: false });
      case 'list_device_sessions':
        return Promise.resolve([]);
      case 'get_network_status':
        return Promise.resolve({ lan_exposure_enabled: false, port: 1992, tls_active: true, exposed_interfaces: [] });
      case 'restore_preferences_backup':
      case 'reset_app_preferences':
      case 'open_preferences_location':
        // Record the call so the test can assert the action was wired to the
        // right command; the void result is the real command's shape.
        return Promise.resolve(
          cmd === 'open_preferences_location' ? null : { preferences: {}, archive_path: null, message: 'ok' },
        );
      default:
        void args;
        return Promise.resolve({});
    }
  });
}

const callsFor = (cmd: string) =>
  vi.mocked(invoke).mock.calls.filter((call) => call[0] === cmd);

beforeEach(() => {
  vi.mocked(invoke).mockReset();
  __resetProviderCachesForTests();
  useToastStore.setState({ toasts: [] });
});

describe('AppSettingsModal — corrupt preferences.json (#1523)', () => {
  it('renders the corruption panel with the reason, the path, and the three recovery actions', async () => {
    mockApp({
      health: { status: 'corrupt', corruption: corruption(), preferences_directory: 'C:\\Users\\test\\AppData\\Roaming\\com.alond.buildmesh' },
    });

    render(<AppSettingsModal onClose={() => {}} />);

    const panel = await screen.findByTestId('preferences-corruption');
    // The user must be able to tell *what happened* and *where*, and must
    // be told explicitly that nothing has been lost yet.
    expect(panel.textContent).toMatch(/couldn’t read your settings file/i);
    expect(panel.textContent).toMatch(/will not overwrite it/i);
    expect(panel.textContent).toMatch(/interrupted write|newer version/i);
    expect(panel.textContent).toContain(CORRUPT_PATH);
    expect(panel.textContent).toContain('812');

    // The actionable triad.
    expect(screen.getByTestId('preferences-restore')).toBeTruthy();
    expect(screen.getByTestId('preferences-open-location')).toBeTruthy();
    expect(screen.getByTestId('preferences-reset')).toBeTruthy();

    // Assertive, named alert — a screen reader user learns about this the
    // moment Settings opens, not on their first failed save.
    expect(panel.getAttribute('role')).toBe('alert');
    expect(panel.getAttribute('aria-label')).toMatch(/settings could not be read/i);
  });

  it('disables every preference-backed control so nothing can be written over the unreadable file', async () => {
    mockApp({
      health: { status: 'corrupt', corruption: corruption(), preferences_directory: 'C:\\x' },
    });

    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);
    await screen.findByTestId('preferences-corruption');

    // The read *succeeded* (status `loaded`), so status alone would leave
    // these enabled — and every one would then be refused by the backend.
    const defaultProvider = await screen.findByLabelText('Default provider');
    await waitFor(() => {
      expect(defaultProvider.hasAttribute('disabled')).toBe(true);
    });
    expect(screen.getByLabelText('Circuit agent pool size').hasAttribute('disabled')).toBe(true);
    expect(screen.getByLabelText('Worktree directory').hasAttribute('disabled')).toBe(true);
    expect(
      screen.getByRole('checkbox', { name: /confirm before quitting/i }).hasAttribute('disabled'),
    ).toBe(true);

    // Providers tab — every account card control writes `provider_accounts`.
    // Without this the user types an API key, hits Save, and gets a raw
    // PREFERENCES_CORRUPT error instead of a disabled control (the first
    // gating pass only covered the General tab).
    await user.click(screen.getByRole('tab', { name: /providers/i }));
    const addProvider = await screen.findByRole('button', { name: /add provider/i });
    expect(addProvider.hasAttribute('disabled')).toBe(true);
    expect(screen.getByRole('checkbox', { name: /^enable /i }).hasAttribute('disabled')).toBe(true);
    expect(
      screen.getAllByRole('button', { name: /^remove /i })[0].hasAttribute('disabled'),
    ).toBe(true);

    // The credential editor is behind a disclosure; the disclosure itself is
    // a local view toggle, so it stays usable — the *write* must not.
    await user.click(screen.getByRole('button', { name: /edit credentials/i }));
    expect((await screen.findByLabelText('MiniMax API key')).hasAttribute('disabled')).toBe(true);
    expect(screen.getByLabelText('MiniMax billing mode').hasAttribute('disabled')).toBe(true);
    expect(screen.getByRole('button', { name: 'Save' }).hasAttribute('disabled')).toBe(true);

    // Launch Configurations tab — reordering persists `harness_order`, and
    // attaching / detaching persists `provider_pairings`.
    await user.click(screen.getByRole('tab', { name: /launch configurations/i }));
    const reorderHandles = await screen.findAllByRole('button', { name: /^reorder /i });
    expect(reorderHandles.length).toBeGreaterThan(1);
    for (const handle of reorderHandles) {
      expect(handle.getAttribute('aria-disabled')).toBe('true');
    }
    expect(screen.getByRole('button', { name: /^detach /i }).hasAttribute('disabled')).toBe(true);
    expect(screen.getByRole('button', { name: /^edit /i }).hasAttribute('disabled')).toBe(true);
  });

  it('a schema mismatch is reported as a forward-version problem, not a damaged file', async () => {
    mockApp({
      health: {
        status: 'corrupt',
        corruption: corruption({
          reason: 'schema_mismatch',
          detail: 'a stored field has a type this Buildmesh version cannot read (unexpected data at line 3 column 30)',
        }),
        preferences_directory: 'C:\\x',
      },
    });

    render(<AppSettingsModal onClose={() => {}} />);
    const panel = await screen.findByTestId('preferences-corruption');
    expect(panel.textContent).toMatch(/newer version/i);
  });

  it('omits the restore action when no usable backup exists rather than offering a dead end', async () => {
    mockApp({
      health: {
        status: 'corrupt',
        corruption: corruption({ backup_available: false, backup_path: null }),
        preferences_directory: 'C:\\x',
      },
    });

    render(<AppSettingsModal onClose={() => {}} />);
    const panel = await screen.findByTestId('preferences-corruption');
    expect(screen.queryByTestId('preferences-restore')).toBeNull();
    expect(panel.textContent).toMatch(/no saved backup is available/i);
    // The other two options remain.
    expect(screen.getByTestId('preferences-open-location')).toBeTruthy();
    expect(screen.getByTestId('preferences-reset')).toBeTruthy();
  });

  it('restore calls the backend and re-reads preferences so the panel clears', async () => {
    let healthy = false;
    mockApp({
      health: { status: 'corrupt', corruption: corruption(), preferences_directory: 'C:\\x' },
      overrides: (cmd) => {
        // Once the restore lands, the backend reports a healthy file.
        if (cmd === 'get_preferences_health' && healthy) {
          return { status: 'healthy', corruption: null, preferences_directory: 'C:\\x' };
        }
        if (cmd === 'restore_preferences_backup') {
          healthy = true;
          // Shaped like the real `recovery::restore_backup` message, which
          // is the contract this test pins: the archive path is the user's
          // only route back to the replaced bytes once the panel is gone.
          return {
            preferences: {},
            archive_path: 'C:\\x\\preferences.json.corrupt-20261004T185500Z',
            message:
              'Restored the last-known-good settings. The unreadable file was kept at C:\\x\\preferences.json.corrupt-20261004T185500Z',
          };
        }
        return undefined;
      },
    });

    render(<AppSettingsModal onClose={() => {}} />);
    fireEvent.click(await screen.findByTestId('preferences-restore'));

    await waitFor(() => {
      expect(callsFor('restore_preferences_backup').length).toBe(1);
    });
    // The recovered preferences are re-read, and the panel goes away because
    // the file is readable again.
    await waitFor(() => {
      expect(screen.queryByTestId('preferences-corruption')).toBeNull();
    });
    expect(callsFor('get_app_preferences').length).toBeGreaterThan(1);
    // The outcome names the archive the replaced bytes went to. The panel is
    // gone by now, so this is the user's only route to it.
    expect(
      useToastStore
        .getState()
        .toasts.map((t) => t.message)
        .join(' '),
    ).toContain('preferences.json.corrupt-20261004T185500Z');
  });

  it('reset is gated behind a confirmation that names what is lost, and cancelling writes nothing', async () => {
    mockApp({
      health: { status: 'corrupt', corruption: corruption(), preferences_directory: 'C:\\x' },
    });

    render(<AppSettingsModal onClose={() => {}} />);

    // Opening the confirm dialog must not itself reset anything.
    fireEvent.click(await screen.findByTestId('preferences-reset'));
    const confirm = await screen.findByRole('button', { name: 'Reset settings' });
    expect(screen.getByText(/Reset settings to defaults\?/)).toBeTruthy();
    // DESIGN.md: a destructive confirmation enumerates the loss.
    const dialog = confirm.closest('div[role="dialog"]') ?? document.body;
    expect(dialog.textContent).toMatch(/provider accounts and API keys/i);
    expect(dialog.textContent).toMatch(/kept as a copy/i);
    expect(callsFor('reset_app_preferences').length).toBe(0);

    // Cancel is the safe default and is focused first.
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    await waitFor(() => {
      expect(screen.queryByRole('button', { name: 'Reset settings' })).toBeNull();
    });
    expect(callsFor('reset_app_preferences').length).toBe(0);
  });

  it('confirming the reset calls the backend reset command', async () => {
    let healthy = false;
    mockApp({
      health: { status: 'corrupt', corruption: corruption(), preferences_directory: 'C:\\x' },
      overrides: (cmd) => {
        if (cmd === 'get_preferences_health' && healthy) {
          return { status: 'healthy', corruption: null, preferences_directory: 'C:\\x' };
        }
        if (cmd === 'reset_app_preferences') {
          healthy = true;
          return { preferences: {}, archive_path: 'C:\\x\\preferences.json.corrupt-20261004T185500Z', message: 'Reset.' };
        }
        return undefined;
      },
    });

    render(<AppSettingsModal onClose={() => {}} />);
    fireEvent.click(await screen.findByTestId('preferences-reset'));
    fireEvent.click(await screen.findByRole('button', { name: 'Reset settings' }));

    await waitFor(() => {
      expect(callsFor('reset_app_preferences').length).toBe(1);
    });
    await waitFor(() => {
      expect(screen.queryByTestId('preferences-corruption')).toBeNull();
    });
  });

  it('a failed recovery keeps the panel up with the error rather than pretending it worked', async () => {
    mockApp({
      health: { status: 'corrupt', corruption: corruption(), preferences_directory: 'C:\\x' },
      overrides: (cmd) =>
        cmd === 'restore_preferences_backup'
          ? Promise.reject(new Error('the last-known-good backup is itself unreadable'))
          : undefined,
    });

    render(<AppSettingsModal onClose={() => {}} />);
    fireEvent.click(await screen.findByTestId('preferences-restore'));

    // The file was not changed by the failed restore, so the user still
    // needs these actions — the panel must not disappear.
    const failure = await screen.findByTestId('preferences-corruption-error');
    expect(failure.textContent).toContain('last-known-good backup is itself unreadable');
    expect(screen.getByTestId('preferences-corruption')).toBeTruthy();
  });

  it('open file location asks the backend to reveal the folder', async () => {
    mockApp({
      health: { status: 'corrupt', corruption: corruption(), preferences_directory: 'C:\\x' },
    });

    render(<AppSettingsModal onClose={() => {}} />);
    fireEvent.click(await screen.findByTestId('preferences-open-location'));
    await waitFor(() => {
      expect(callsFor('open_preferences_location').length).toBe(1);
    });
    // Revealing the folder is not a recovery — the panel stays.
    expect(screen.getByTestId('preferences-corruption')).toBeTruthy();
  });

  it('a healthy file renders no panel and leaves settings controls enabled', async () => {
    mockApp({
      health: { status: 'healthy', corruption: null, preferences_directory: 'C:\\x' },
    });

    render(<AppSettingsModal onClose={() => {}} />);
    await screen.findByLabelText('Default provider');
    await waitFor(() => {
      expect(screen.queryByTestId('preferences-corruption')).toBeNull();
    });
    expect(screen.getByLabelText('Circuit agent pool size').hasAttribute('disabled')).toBe(false);
  });

  it('a missing file is not a corruption state', async () => {
    mockApp({
      health: { status: 'missing', corruption: null, preferences_directory: 'C:\\x' },
    });

    render(<AppSettingsModal onClose={() => {}} />);
    await screen.findByLabelText('Default provider');
    await waitFor(() => {
      expect(screen.queryByTestId('preferences-corruption')).toBeNull();
    });
  });

  it('a failed health probe does not disable a readable preferences file', async () => {
    // The health call is best-effort. If it cannot be made, the user loses
    // the recovery panel — but the backend still refuses writes to a
    // corrupt file, so the cost is a disabled control, never data loss.
    mockApp({
      health: null,
      overrides: (cmd) =>
        cmd === 'get_preferences_health'
          ? Promise.reject(new Error('health probe unavailable'))
          : undefined,
    });

    render(<AppSettingsModal onClose={() => {}} />);
    await screen.findByLabelText('Default provider');
    await waitFor(() => {
      expect(screen.queryByTestId('preferences-corruption')).toBeNull();
    });
    expect(screen.getByLabelText('Circuit agent pool size').hasAttribute('disabled')).toBe(false);
  });
});
