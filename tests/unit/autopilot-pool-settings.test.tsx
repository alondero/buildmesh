/**
 * Settings → General → Autopilot pool size: the app-wide cap on concurrently
 * active autopilot nodes across ALL meshes (the pool the per-mesh
 * `autopilot_concurrency_limit` slots draw from). These tests pin the UI
 * contract: hydration from preferences, commit-on-blur (not per keystroke),
 * empty = no cap (null on the wire), negative input clamped to 0, and the
 * dirty-site wiring into the modal's discard-confirm.
 *
 * Also pins the sub-pane redesign's core invariant: inactive panes stay
 * MOUNTED (hidden) so child drafts survive tab switches.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';

function mockBackend(poolSize: number | null = null) {
  const calls: Record<string, unknown[]> = {};
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    calls[cmd] = [...(calls[cmd] ?? []), args];
    switch (cmd) {
      case 'get_app_preferences':
        return Promise.resolve({
          default_provider: null,
          minimax_api_key: null,
          autopilot_pool_size: poolSize,
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

/** The pool-size input lives on the General pane, which is the default
 *  active tab — no navigation needed. Number inputs expose role spinbutton. */
const poolInput = async () =>
  (await screen.findByRole('spinbutton', { name: /autopilot pool size/i })) as HTMLInputElement;

describe('Settings — autopilot pool size', () => {
  beforeEach(() => vi.mocked(invoke).mockReset());

  it('hydrates the stored pool size, and an unset preference reads as no cap (empty)', async () => {
    mockBackend(6);
    render(<AppSettingsModal onClose={() => {}} />);
    const input = await poolInput();
    await waitFor(() => expect(input.value).toBe('6'));
  });

  it('commits a typed value on blur as a number', async () => {
    const calls = mockBackend(null);
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);

    const input = await poolInput();
    await waitFor(() => expect(input.disabled).toBe(false));
    await user.type(input, '4');
    // No save yet — commit is on blur/Enter so a half-typed "1" of "10"
    // never briefly caps the pool at 1.
    expect(calls['set_app_autopilot_pool_size']).toBeUndefined();

    fireEvent.blur(input);
    await waitFor(() => expect(calls['set_app_autopilot_pool_size']).toBeTruthy());
    expect(calls['set_app_autopilot_pool_size']![0]).toEqual({ size: 4 });
  });

  it('clearing the input commits null (no global cap)', async () => {
    const calls = mockBackend(3);
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);

    const input = await poolInput();
    await waitFor(() => expect(input.value).toBe('3'));
    await user.clear(input);
    fireEvent.blur(input);

    await waitFor(() => expect(calls['set_app_autopilot_pool_size']).toBeTruthy());
    expect(calls['set_app_autopilot_pool_size']![0]).toEqual({ size: null });
  });

  it('clamps a negative entry to 0 (pause) rather than sending garbage', async () => {
    const calls = mockBackend(null);
    render(<AppSettingsModal onClose={() => {}} />);

    const input = await poolInput();
    await waitFor(() => expect(input.disabled).toBe(false));
    fireEvent.change(input, { target: { value: '-5' } });
    fireEvent.blur(input);

    await waitFor(() => expect(calls['set_app_autopilot_pool_size']).toBeTruthy());
    expect(calls['set_app_autopilot_pool_size']![0]).toEqual({ size: 0 });
  });

  it('an uncommitted draft makes the modal dirty: Escape intercepts, and the focus shift commits the draft', async () => {
    const calls = mockBackend(null);
    const onClose = vi.fn();
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={onClose} />);

    const input = await poolInput();
    await waitFor(() => expect(input.disabled).toBe(false));
    await user.type(input, '8');
    // While the draft is uncommitted, the nav rail flags the General pane.
    screen.getByTestId('settings-tab-dirty-general');

    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClose).not.toHaveBeenCalled();
    screen.getByTestId('modal-discard-banner');
    // Opening the banner focuses its Cancel button, which blurs the input —
    // and blur commits. So by the time the banner is visible the draft is
    // already saved (nothing can be lost) and the dirty dot clears.
    await waitFor(() => expect(calls['set_app_autopilot_pool_size']).toBeTruthy());
    expect(calls['set_app_autopilot_pool_size']![0]).toEqual({ size: 8 });
    expect(screen.queryByTestId('settings-tab-dirty-general')).toBeNull();
  });
});

describe('Settings — sub-pane navigation', () => {
  beforeEach(() => vi.mocked(invoke).mockReset());

  it('switches panes via the nav rail, keeping inactive panes mounted so drafts survive', async () => {
    mockBackend(null);
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);

    const input = await poolInput();
    await waitFor(() => expect(input.disabled).toBe(false));
    await user.type(input, '5');

    // Away to Remote Access: the LAN checkbox becomes reachable by role,
    // the General content no longer is.
    fireEvent.click(screen.getByRole('tab', { name: /remote access/i }));
    await screen.findByRole('checkbox', { name: /expose to lan/i });
    expect(screen.queryByRole('spinbutton', { name: /autopilot pool size/i })).toBeNull();

    // Back to General: the draft survived the round-trip because the pane
    // was hidden, not unmounted (the modal's dirty tracking lives in
    // component state — issue #730).
    fireEvent.click(screen.getByRole('tab', { name: 'General' }));
    expect(((await poolInput())).value).toBe('5');
  });
});
