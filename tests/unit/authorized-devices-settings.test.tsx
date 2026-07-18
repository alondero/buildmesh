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
 *
 * `options.rejectRevoke` makes `revoke_device_session` reject (for rollback
 * tests). `options.listAfterRevoke` is a one-shot override returned by the
 * NEXT `list_device_sessions` call after a successful revoke — it lets the
 * "refresh after revoke" test inject a device that re-paired between the
 * optimistic remove and the post-success refresh. `options.rejectListAfterRevoke`
 * makes that same next list call reject — pins the best-effort refresh path.
 */
function mockBackend(
  devices: DeviceSession[],
  options?: {
    rejectRevoke?: Error;
    listAfterRevoke?: DeviceSession[];
    rejectListAfterRevoke?: Error;
  },
) {
  let list = [...devices];
  // Both are one-shots set by a successful revoke and consumed by the very
  // next list call. Keeping them separate (instead of a discriminated union)
  // avoids a value/error wrapper just for the mock.
  let nextListOverride: DeviceSession[] | null = null;
  let nextListRejection: Error | null = null;
  const calls: Record<string, unknown[]> = {};
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    calls[cmd] = [...(calls[cmd] ?? []), args];
    switch (cmd) {
      case 'list_device_sessions': {
        // Consume one-shots on the first read so a second revoke + refresh
        // cycle in the same test stays honest.
        if (nextListOverride) {
          const snapshot = nextListOverride;
          nextListOverride = null;
          return Promise.resolve(snapshot);
        }
        if (nextListRejection) {
          const err = nextListRejection;
          nextListRejection = null;
          return Promise.reject(err);
        }
        return Promise.resolve(list);
      }
      case 'revoke_device_session':
        if (options?.rejectRevoke) {
          // Reject BEFORE mutating `list` — a failed revoke must leave the
          // mock in the same shape the component believes (rolled-back).
          return Promise.reject(options.rejectRevoke);
        }
        list = list.filter(d => d.id !== (args as { id: number }).id);
        if (options?.listAfterRevoke) nextListOverride = options.listAfterRevoke;
        if (options?.rejectListAfterRevoke) nextListRejection = options.rejectListAfterRevoke;
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

  it('re-fetches the device list after a successful revoke (issue #595)', async () => {
    // A phone re-pairs between the optimistic remove and the post-success
    // refresh — the panel must show it without the user reopening the modal.
    const calls = mockBackend(
      [device({ id: 11, label: 'Chrome on Android', last_ip: '192.168.1.9' })],
      { listAfterRevoke: [device({ id: 12, label: 'Safari on iPhone', last_ip: '10.0.0.7' })] },
    );
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/remote access/i);

    await screen.findByText('Chrome on Android');
    await user.click(screen.getByRole('button', { name: /^revoke$/i }));
    await user.click(screen.getByRole('button', { name: /confirm revoke/i }));

    await waitFor(() => expect(calls['revoke_device_session']).toBeTruthy());
    // Refresh fires after the revoke succeeds — exactly one extra list call
    // for a single revoke (initial load + post-success refresh).
    await waitFor(() => expect(calls['list_device_sessions']?.length).toBeGreaterThanOrEqual(2));
    // The re-paired device surfaces, the revoked one stays gone.
    await screen.findByText('Safari on iPhone');
    expect(screen.queryByText('Chrome on Android')).toBeNull();
  });

  it('rolls the row back and surfaces the error when the backend rejects (issue #595)', async () => {
    const calls = mockBackend(
      [device({ id: 13, label: 'Chrome on Android', last_ip: '192.168.1.9' })],
      { rejectRevoke: new Error('device session not found') },
    );
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/remote access/i);

    await screen.findByText('Chrome on Android');
    await user.click(screen.getByRole('button', { name: /^revoke$/i }));
    await user.click(screen.getByRole('button', { name: /confirm revoke/i }));

    await waitFor(() => expect(calls['revoke_device_session']).toBeTruthy());
    // Optimistic remove ran first — the row must be restored on rejection so
    // the list never lies about what's still authorized.
    await waitFor(() => expect(screen.getByText('Chrome on Android')).toBeTruthy());
    expect(screen.getByText(/device session not found/i)).toBeTruthy();
  });

  it('leaves the optimistic remove in place when the post-success refresh fails', async () => {
    // The revoke succeeded on the backend, but the follow-up list-fetch
    // failed (transient DB hiccup, dropped connection, etc.). Reverting the
    // row would be a worse lie than briefly stale metadata — the device IS
    // revoked, so keep it off-screen. The failure is logged, not surfaced.
    const calls = mockBackend(
      [device({ id: 14, label: 'Chrome on Android', last_ip: '192.168.1.9' })],
      { rejectListAfterRevoke: new Error('refresh failed') },
    );
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);
    await openSettingsPane(/remote access/i);

    await screen.findByText('Chrome on Android');
    await user.click(screen.getByRole('button', { name: /^revoke$/i }));
    await user.click(screen.getByRole('button', { name: /confirm revoke/i }));

    await waitFor(() => expect(calls['revoke_device_session']).toBeTruthy());
    // Wait for the refresh call to fire and fail.
    await waitFor(() => expect(calls['list_device_sessions']?.length).toBeGreaterThanOrEqual(2));
    // Row stays gone — no rollback of a successful revoke.
    await waitFor(() => expect(screen.queryByText('Chrome on Android')).toBeNull());
    // No user-visible error — the refresh failure is non-fatal by design.
    expect(screen.queryByText(/refresh failed/i)).toBeNull();
  });
});
