import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor } from '@testing-library/react';
import { openSettingsPane } from '../utils/settings-panes';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import type { DeviceSession } from '../../src/types/generated/DeviceSession';

/**
 * Route invoke() by command name. Only the device commands matter here;
 * everything else resolves to a benign default so the modal mounts cleanly.
 */
function mockBackend(devices: DeviceSession[]) {
  let list = [...devices];
  const calls: Record<string, unknown[]> = {};
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    calls[cmd] = [...(calls[cmd] ?? []), args];
    switch (cmd) {
      case 'list_device_sessions':
        return Promise.resolve(list);
      case 'revoke_device_session':
        list = list.filter(d => d.id !== (args as { id: number }).id);
        return Promise.resolve(undefined);
      case 'get_app_preferences':
        return Promise.resolve({ default_provider: null, minimax_api_key: null });
      case 'list_providers':
        return Promise.resolve([]);
      case 'get_provider_accounts':
        return Promise.resolve([]);
      case 'get_provider_meters':
        return Promise.resolve([]);
      case 'get_coordinator_status':
        return Promise.resolve({ enabled: false, has_token: false });
      default:
        return Promise.resolve({});
    }
  });
  return calls;
}

const device = (over: Partial<DeviceSession>): DeviceSession => ({
  id: 1,
  label: 'Safari on iPhone',
  last_ip: '10.0.0.5',
  created_at: '2026-06-20 10:00:00',
  last_active_at: '2026-06-22 09:30:00',
  ...over,
});

describe('Authorized Devices settings section', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
  });

  it('lists each paired device with its label, IP and last-active time', async () => {
    mockBackend([device({ id: 1, label: 'Safari on iPhone', last_ip: '10.0.0.5' })]);
    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/remote access/i);

    await screen.findByText('Safari on iPhone');
    expect(screen.getByText(/10\.0\.0\.5/)).toBeTruthy();
    expect(screen.getByText(/2026-06-22 09:30:00/)).toBeTruthy();
  });

  it('shows an empty state when no devices are paired', async () => {
    mockBackend([]);
    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/remote access/i);

    await screen.findByText(/no paired devices yet/i);
  });

  it('revokes a device through a two-step confirm and removes the row', async () => {
    const calls = mockBackend([
      device({ id: 7, label: 'Chrome on Android', last_ip: '192.168.1.9' }),
    ]);
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/remote access/i);

    await screen.findByText('Chrome on Android');

    // First click only arms the confirm — no backend call yet.
    await user.click(screen.getByRole('button', { name: /^revoke$/i }));
    expect(calls['revoke_device_session']).toBeUndefined();

    // Confirm fires the revoke and the row disappears.
    await user.click(screen.getByRole('button', { name: /confirm revoke/i }));
    await waitFor(() => expect(calls['revoke_device_session']).toBeTruthy());
    expect((calls['revoke_device_session']![0] as { id: number }).id).toBe(7);

    await waitFor(() => expect(screen.queryByText('Chrome on Android')).toBeNull());
  });

  it('lets the user cancel out of the confirm without revoking', async () => {
    const calls = mockBackend([device({ id: 3 })]);
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/remote access/i);

    await screen.findByText('Safari on iPhone');
    await user.click(screen.getByRole('button', { name: /^revoke$/i }));
    await user.click(screen.getByRole('button', { name: /cancel/i }));

    expect(calls['revoke_device_session']).toBeUndefined();
    // Row is still listed.
    expect(screen.getByText('Safari on iPhone')).toBeTruthy();
  });
});
