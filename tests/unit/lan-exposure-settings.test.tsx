import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { openSettingsPane } from '../utils/settings-panes';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';

/**
 * Route invoke() by command name for the LAN/VPN exposure section (issue #501).
 * Everything outside that section resolves to a benign default so the modal
 * mounts cleanly; `get_network_status` carries the toggle state under test.
 * Issue #586 adds `tls_active` + `exposed_interfaces` so the UI can report
 * realized exposure (loopback-only vs LAN interfaces actually bound).
 */
function mockBackend(
  lanEnabled = false,
  tlsActive = false,
  exposedInterfaces: { address: string; tls: boolean }[] = [],
  devices: { id: number; label: string; last_ip: string | null; last_active_at: string }[] = [],
) {
  const state = { lanEnabled, tlsActive, exposedInterfaces, devices };
  const calls: Record<string, unknown[]> = {};
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    calls[cmd] = [...(calls[cmd] ?? []), args];
    switch (cmd) {
      case 'get_network_status':
        return Promise.resolve({
          lan_exposure_enabled: state.lanEnabled,
          port: 1992,
          tls_active: state.tlsActive,
          exposed_interfaces: state.exposedInterfaces,
        });
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
        // A NEW array each call, mirroring the real loader — that fresh
        // identity is what makes the adoption guard fire again.
        return Promise.resolve([...state.devices]);
      case 'revoke_device_session':
        state.devices = state.devices.filter((d) => d.id !== args?.id);
        return Promise.resolve(undefined);
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
    await openSettingsPane(/remote access/i);

    const toggle = await screen.findByRole('checkbox', { name: /expose to lan/i });
    expect((toggle as HTMLInputElement).checked).toBe(false);
  });

  it('makes the self-signed-TLS / loopback-default nature clear', async () => {
    mockBackend(false);
    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/remote access/i);

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
    await openSettingsPane(/remote access/i);

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
    await openSettingsPane(/remote access/i);

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
          return Promise.resolve({
            lan_exposure_enabled: false,
            port: 1992,
            tls_active: false,
            exposed_interfaces: [],
          });
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
    await openSettingsPane(/remote access/i);

    const toggle = await screen.findByRole('checkbox', { name: /expose to lan/i });
    await user.click(toggle);

    // Optimistic flip, then rollback to false once the backend rejects.
    await waitFor(() => expect((toggle as HTMLInputElement).checked).toBe(false));
    expect(screen.getByText(/bind failed/i)).toBeTruthy();
  });
});

// Realized exposure (issue #586): the toggle can be ON while the server is
// still loopback-only (TLS init failure, no non-loopback interface, or a
// per-interface bind error). These tests pin the UI's "exposed but not
// reachable" signal so the user isn't sent a dead URL.
describe('Realized LAN exposure status (issue #586)', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it('lists the actually-bound interfaces when LAN exposure is healthy', async () => {
    mockBackend(true, true, [
      { address: '192.168.1.5:1992', tls: true },
      { address: '10.0.0.2:1992', tls: true },
    ]);
    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/remote access/i);

    await screen.findByRole('checkbox', { name: /expose to lan/i });
    const items = await screen.findAllByTestId('lan-exposed-interface');
    expect(items).toHaveLength(2);
    expect(items[0].textContent).toMatch(/192\.168\.1\.5:1992/);
    expect(items[0].textContent).toMatch(/HTTPS\/WSS/);
    expect(items[1].textContent).toMatch(/10\.0\.0\.2:1992/);
    // Healthy state — no warning, no TLS-missing notice.
    expect(screen.queryByTestId('lan-exposure-warning')).toBeNull();
    expect(screen.queryByTestId('lan-tls-warning')).toBeNull();
  });

  it('warns when the toggle is on but no interfaces are exposed', async () => {
    // TLS init failed (or no non-loopback interface): DB intent says enabled,
    // but realized = loopback only. The UI must say so.
    mockBackend(true, false, []);
    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/remote access/i);

    await screen.findByRole('checkbox', { name: /expose to lan/i });
    const warning = await screen.findByTestId('lan-exposure-warning');
    expect(warning.textContent).toMatch(/no interfaces are actually exposed/i);
    expect(warning.getAttribute('role')).toBe('alert');
    expect(screen.queryByTestId('lan-exposed-interface')).toBeNull();
  });

  it('warns when interfaces bound without TLS (TLS init failed mid-cycle)', async () => {
    // Belt-and-braces: if for some reason a non-loopback listener bound
    // without TLS, surface that as its own alert so the user knows the
    // connection is unencrypted.
    mockBackend(true, false, [{ address: '192.168.1.5:1992', tls: false }]);
    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/remote access/i);

    await screen.findByRole('checkbox', { name: /expose to lan/i });
    const items = await screen.findAllByTestId('lan-exposed-interface');
    expect(items).toHaveLength(1);
    expect(items[0].textContent).toMatch(/plain/);
    const tlsWarn = await screen.findByTestId('lan-tls-warning');
    expect(tlsWarn.textContent).toMatch(/tls is not active/i);
    expect(tlsWarn.getAttribute('role')).toBe('alert');
  });

  it('renders no realized-state UI when the toggle is off', async () => {
    mockBackend(false, false, []);
    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/remote access/i);

    await screen.findByRole('checkbox', { name: /expose to lan/i });
    // The realized-status block is gated on `lanEnabled`, so even though the
    // backend would report empty lists, nothing renders for the off case.
    expect(screen.queryByTestId('lan-realized-status')).toBeNull();
    expect(screen.queryByTestId('lan-exposure-warning')).toBeNull();
    expect(screen.queryByTestId('lan-exposed-interface')).toBeNull();
  });

  // Regression test for #1880 review finding 1. The three Remote Access
  // payloads (devices / coordinator / network) originally shared ONE adoption
  // guard, so any one of them arriving re-ran all three adoptions. A devices
  // refresh would therefore re-seed `lanEnabled` from the network payload,
  // clobbering the optimistic value a just-issued LAN toggle owns — the
  // toggle visibly snaps back without the user having touched it.
  //
  // The trigger here is a real user action on a sibling control (confirming a
  // device revoke re-reads devices) rather than a direct `invoke` call,
  // because only the loader path publishes a new payload.
  it('keeps a LAN toggle optimistic when a sibling control re-reads devices', async () => {
    const phone = { id: 7, label: 'Pixel', last_ip: '192.168.1.5', last_active_at: 'a minute ago' };
    mockBackend(false, false, [], [phone]);
    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/remote access/i);

    const toggle = (await screen.findByRole('checkbox', { name: /expose to lan/i })) as HTMLInputElement;

    // Enable exposure. `set_lan_exposure_enabled` is optimistic, but no
    // network payload has been re-read yet, so `lanEnabled` is owned by the
    // toggle.
    await userEvent.click(toggle);
    await waitFor(() => expect(toggle.checked).toBe(true));

    // Drive an unrelated payload: revoking the paired device calls
    // `loadDevices()`, publishing a NEW devices array. Under the combined
    // guard that re-ran the network adoption and reset `lanEnabled` back to
    // the last-read `false`.
    await userEvent.click(await screen.findByRole('button', { name: 'Revoke' }));
    await userEvent.click(await screen.findByRole('button', { name: 'Confirm revoke' }));
    await waitFor(() =>
      expect(invoke).toHaveBeenCalledWith('revoke_device_session', { id: phone.id }),
    );

    expect(toggle.checked).toBe(true);
  });
});
