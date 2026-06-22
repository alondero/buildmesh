import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';

/**
 * Route invoke() by command name for the LAN/VPN exposure section (issue #501).
 * Everything outside that section resolves to a benign default so the modal
 * mounts cleanly; `get_network_status` carries the toggle state under test.
 */
function mockBackend(lanEnabled = false) {
  const state = { lanEnabled };
  const calls: Record<string, unknown[]> = {};
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    calls[cmd] = [...(calls[cmd] ?? []), args];
    switch (cmd) {
      case 'get_network_status':
        return Promise.resolve({ lan_exposure_enabled: state.lanEnabled, port: 1992 });
      case 'set_lan_exposure_enabled':
        state.lanEnabled = Boolean(args?.enabled);
        return Promise.resolve(undefined);
      case 'get_coordinator_status':
        return Promise.resolve({ enabled: false, has_token: false });
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
      default:
        return Promise.resolve({});
    }
  });
  return calls;
}

describe('LAN / VPN exposure settings section (issue #501)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it('shows exposure as disabled by default', async () => {
    mockBackend(false);
    render(<AppSettingsModal onClose={() => {}} />);

    const toggle = await screen.findByRole('checkbox', { name: /expose to lan/i });
    expect((toggle as HTMLInputElement).checked).toBe(false);
  });

  it('makes the self-signed-TLS / loopback-default nature clear', async () => {
    mockBackend(false);
    render(<AppSettingsModal onClose={() => {}} />);

    await screen.findByRole('checkbox', { name: /expose to lan/i });
    // "self-signed" and "loopback" each appear in more than one place (the
    // explainer copy, the toggle label, the coordinator section), so assert
    // presence with getAllByText rather than the single-match getByText.
    expect(screen.getAllByText(/self-signed/i).length).toBeGreaterThan(0);
    expect(screen.getAllByText(/loopback/i).length).toBeGreaterThan(0);
  });

  it('enables exposure, calling the backend to rebind the listeners', async () => {
    const calls = mockBackend(false);
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);

    const toggle = await screen.findByRole('checkbox', { name: /expose to lan/i });
    await user.click(toggle);

    await waitFor(() => expect(calls['set_lan_exposure_enabled']).toBeTruthy());
    expect((calls['set_lan_exposure_enabled']![0] as { enabled: boolean }).enabled).toBe(true);
    await waitFor(() => expect((toggle as HTMLInputElement).checked).toBe(true));
  });

  it('reflects an already-enabled setting on open and can turn it off', async () => {
    const calls = mockBackend(true);
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);

    const toggle = await screen.findByRole('checkbox', { name: /expose to lan/i });
    await waitFor(() => expect((toggle as HTMLInputElement).checked).toBe(true));

    await user.click(toggle);
    await waitFor(() => expect(calls['set_lan_exposure_enabled']).toBeTruthy());
    expect((calls['set_lan_exposure_enabled']![0] as { enabled: boolean }).enabled).toBe(false);
  });

  it('rolls the toggle back if the backend rejects', async () => {
    const calls: Record<string, unknown[]> = {};
    vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
      calls[cmd] = [...(calls[cmd] ?? []), args];
      switch (cmd) {
        case 'get_network_status':
          return Promise.resolve({ lan_exposure_enabled: false, port: 1992 });
        case 'set_lan_exposure_enabled':
          return Promise.reject(new Error('bind failed'));
        case 'get_coordinator_status':
          return Promise.resolve({ enabled: false, has_token: false });
        case 'get_app_preferences':
          return Promise.resolve({ default_provider: null, minimax_api_key: null });
        case 'list_providers':
        case 'get_provider_accounts':
        case 'get_provider_meters':
        case 'list_device_sessions':
          return Promise.resolve([]);
        default:
          return Promise.resolve({});
      }
    });
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);

    const toggle = await screen.findByRole('checkbox', { name: /expose to lan/i });
    await user.click(toggle);

    // Optimistic flip, then rollback to false once the backend rejects.
    await waitFor(() => expect((toggle as HTMLInputElement).checked).toBe(false));
    expect(screen.getByText(/bind failed/i)).toBeTruthy();
  });
});
