/**
 * Settings → General → Worktree directory (issue #1519): the Buildmesh-wide
 * default folder for new Worktree Nodes. Pins the UI contract: hydration
 * from preferences, commit-on-blur (not per keystroke), empty = default
 * `.claude/worktrees` (null on the wire), and trimming.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';

function mockBackend(worktreeDir: string | null = null) {
  const calls: Record<string, unknown[]> = {};
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    calls[cmd] = [...(calls[cmd] ?? []), args];
    switch (cmd) {
      case 'get_app_preferences':
        return Promise.resolve({
          default_provider: null,
          minimax_api_key: null,
          autopilot_pool_size: null,
          worktree_directory: worktreeDir,
          harness_defaults: {},
        });
      case 'get_coordinator_status':
        return Promise.resolve({ enabled: false, has_token: false });
      case 'get_network_status':
        return Promise.resolve({
          lan_exposure_enabled: false,
          port: 1992,
          tls_active: false,
          exposed_interfaces: [],
        });
      case 'list_providers':
      case 'get_provider_accounts':
      case 'get_provider_meters':
      case 'list_device_sessions':
        return Promise.resolve([]);
      default:
        return Promise.resolve({});
    }
  });
  return calls;
}

const dirInput = async () =>
  (await screen.findByRole('textbox', { name: /worktree directory/i })) as HTMLInputElement;

describe('Settings — worktree directory', () => {
  beforeEach(() => vi.mocked(invoke).mockReset());

  it('hydrates the stored directory, empty when unset', async () => {
    mockBackend('custom-wt');
    render(<AppSettingsModal onClose={() => {}} />);
    const input = await dirInput();
    await waitFor(() => expect(input.value).toBe('custom-wt'));
  });

  it('commits a typed relative value on blur, trimmed', async () => {
    const calls = mockBackend(null);
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);

    const input = await dirInput();
    await waitFor(() => expect(input.disabled).toBe(false));
    await user.type(input, '  my-wt  ');
    expect(calls['set_app_worktree_directory']).toBeUndefined();

    fireEvent.blur(input);
    await waitFor(() => expect(calls['set_app_worktree_directory']).toBeTruthy());
    expect(calls['set_app_worktree_directory']![0]).toEqual({ directory: 'my-wt' });
  });

  it('clearing the input commits null (restore default)', async () => {
    const calls = mockBackend('old-wt');
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);

    const input = await dirInput();
    await waitFor(() => expect(input.value).toBe('old-wt'));
    await user.clear(input);
    fireEvent.blur(input);

    await waitFor(() => expect(calls['set_app_worktree_directory']).toBeTruthy());
    expect(calls['set_app_worktree_directory']![0]).toEqual({ directory: null });
  });
});
