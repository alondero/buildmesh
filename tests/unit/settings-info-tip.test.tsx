/**
 * Settings rows keep their long help text behind the InfoTip (ⓘ) affordance:
 * the explanation is absent until the trigger is activated, and Escape dismisses
 * only the tip — not the surrounding modal.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';

function mockBackend() {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    switch (cmd) {
      case 'get_app_preferences':
        return Promise.resolve({
          default_provider: null,
          reviewer_provider: null,
          naming_provider: null,
          autopilot_pool_size: null,
          worktree_directory: null,
          harness_defaults: {},
          provider_pairings: [],
          confirm_before_quit: true,
        });
      case 'get_coordinator_status':
        return Promise.resolve({ enabled: false, has_token: false });
      case 'get_network_status':
        return Promise.resolve({
          lan_exposure_enabled: false,
          tls_active: false,
          exposed_interfaces: [],
        });
      case 'list_providers':
      case 'get_provider_accounts':
      case 'get_keyed_first_class_catalog':
      case 'list_device_sessions':
        return Promise.resolve([]);
      default:
        return Promise.resolve({});
    }
  });
}

describe('Settings — InfoTip help affordance', () => {
  beforeEach(() => vi.mocked(invoke).mockReset());

  it('hides the explanation until the ⓘ is activated, then Escape dismisses just the tip', async () => {
    mockBackend();
    render(<AppSettingsModal onClose={() => {}} />);

    const trigger = await screen.findByRole('button', { name: 'About Autopilot pool size' });
    // Closed by default — the long paragraph is not rendered at all.
    expect(screen.queryByRole('tooltip')).toBeNull();

    fireEvent.click(trigger);
    const tip = await screen.findByRole('tooltip');
    expect(tip.textContent).toMatch(/leave empty for no global cap/i);
    expect(tip.textContent).toMatch(/lowering the cap just holds new spawns/i);
    expect(trigger.getAttribute('aria-expanded')).toBe('true');

    // Escape closes the tip only — the modal stays mounted, and focus walks
    // back to the ⓘ trigger (the disclosure contract).
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('tooltip')).toBeNull());
    expect(screen.getByRole('dialog')).toBeTruthy();
    await waitFor(() => expect(document.activeElement).toBe(trigger));
  });
});
