import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { openSettingsPane } from '../utils/settings-panes';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';

interface CoordinatorState {
  enabled: boolean;
  hasToken: boolean;
}

/**
 * Route invoke() by command name. The coordinator section only cares about the
 * three coordinator commands; everything else (provider list, usage, prefs)
 * resolves to a benign default so the modal mounts cleanly.
 */
function mockBackend(coordinator: CoordinatorState = { enabled: false, hasToken: false }) {
  const state = { ...coordinator };
  const calls: Record<string, unknown[]> = {};
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    calls[cmd] = [...(calls[cmd] ?? []), args];
    switch (cmd) {
      case 'get_coordinator_status':
        return Promise.resolve({ enabled: state.enabled, has_token: state.hasToken });
      case 'set_coordinator_api_enabled':
        state.enabled = Boolean(args?.enabled);
        return Promise.resolve(undefined);
      case 'generate_coordinator_read_token':
        state.hasToken = true;
        return Promise.resolve('deadbeef1234567890abcdef12345678');
      case 'get_app_preferences':
        return Promise.resolve({ default_provider: null, minimax_api_key: null });
      case 'list_providers':
        return Promise.resolve([]);
      case 'get_provider_accounts':
        return Promise.resolve([]);
      case 'get_provider_meters':
        return Promise.resolve([]);
      case 'list_device_sessions':
        return Promise.resolve([]);
      case 'get_network_status':
        return Promise.resolve({
          lan_exposure_enabled: false,
          port: 1992,
          tls_active: false,
          exposed_interfaces: [],
        });
      default:
        return Promise.resolve({});
    }
  });
  return calls;
}

describe('Coordinator Read API settings section', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it('shows the coordinator API as disabled by default', async () => {
    mockBackend({ enabled: false, hasToken: false });
    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/remote access/i);

    const toggle = await screen.findByRole('checkbox', { name: /coordinator read api/i });
    expect((toggle as HTMLInputElement).checked).toBe(false);
  });

  it('makes the loopback/LAN-bound, own-tunnel nature clear', async () => {
    mockBackend({ enabled: false, hasToken: false });
    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/remote access/i);

    await screen.findByRole('checkbox', { name: /coordinator read api/i });
    // "loopback" now also appears in the LAN/VPN exposure section (issue #501),
    // so match presence rather than a single occurrence; "tunnel" stays unique
    // to the coordinator copy.
    expect(screen.getAllByText(/loopback/i).length).toBeGreaterThan(0);
    expect(screen.getByText(/tunnel/i)).toBeTruthy();
  });

  it('enables the API and mints a read-scoped token', async () => {
    const calls = mockBackend({ enabled: false, hasToken: false });
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/remote access/i);

    const toggle = await screen.findByRole('checkbox', { name: /coordinator read api/i });
    await user.click(toggle);
    await waitFor(() => expect(calls['set_coordinator_api_enabled']).toBeTruthy());
    expect((calls['set_coordinator_api_enabled']![0] as { enabled: boolean }).enabled).toBe(true);

    const mintBtn = await screen.findByRole('button', { name: /generate token/i });
    await user.click(mintBtn);

    await waitFor(() =>
      expect(screen.getByDisplayValue('deadbeef1234567890abcdef12345678')).toBeTruthy(),
    );
  });

  it('copies the minted token to the clipboard', async () => {
    mockBackend({ enabled: true, hasToken: false });
    // userEvent installs its own jsdom clipboard stub; read it back to assert.
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/remote access/i);

    const mintBtn = await screen.findByRole('button', { name: /generate token/i });
    await user.click(mintBtn);
    await screen.findByDisplayValue('deadbeef1234567890abcdef12345678');

    const copyBtn = screen.getByRole('button', { name: /copy/i });
    await user.click(copyBtn);

    await waitFor(async () =>
      expect(await navigator.clipboard.readText()).toBe('deadbeef1234567890abcdef12345678'),
    );
  });

  it('flips the kill-switch off, calling the backend to reject further requests', async () => {
    const calls = mockBackend({ enabled: true, hasToken: true });
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/remote access/i);

    const toggle = await screen.findByRole('checkbox', { name: /coordinator read api/i });
    expect((toggle as HTMLInputElement).checked).toBe(true);

    await user.click(toggle);
    await waitFor(() => expect(calls['set_coordinator_api_enabled']).toBeTruthy());
    expect((calls['set_coordinator_api_enabled']![0] as { enabled: boolean }).enabled).toBe(false);
  });
});
