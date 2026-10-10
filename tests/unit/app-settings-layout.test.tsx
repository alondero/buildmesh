/**
 * Settings dialog layout rework: which tab owns which setting, and that the
 * rarely-touched or self-explanatory text is gone from the page.
 *
 * Tab membership is asserted through the tabpanel that actually contains the
 * control (`closest('[role=tabpanel]')` → `aria-labelledby` → tab label), not
 * through "is it visible", so a setting that lands on the wrong tab fails even
 * though every pane stays mounted.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import { openSettingsPane } from '../utils/settings-panes';
import type { ProviderInfo } from '../../src/types/generated/ProviderInfo';

function row(harness_id: string, over: Partial<ProviderInfo['capabilities']> = {}): ProviderInfo {
  return {
    id: harness_id,
    label: harness_id.charAt(0).toUpperCase() + harness_id.slice(1),
    color: '',
    icon: '',
    resumable: false,
    harness_id,
    provider_id: null,
    is_proxied: false,
    group_key: harness_id,
    capabilities: {
      harness_id,
      supports_resume: false,
      auto_resume_on_startup: false,
      requires_attention_hook: false,
      produces_readable_transcript: false,
      supports_model_override: true,
      supports_effort_override: true,
      supports_prefill: false,
      is_plain_terminal: harness_id === 'terminal',
      effort_control: { kind: 'closed', allowed: ['low', 'high', 'max'] },
      permission_modes: [],
      default_permission_mode: null,
      available_on: ['windows'],
      ...over,
    },
  } as ProviderInfo;
}

const PROVIDERS = [
  row('claude'),
  row('codex'),
  row('terminal', {
    supports_model_override: false,
    supports_effort_override: false,
    effort_control: { kind: 'none' },
  }),
];

function mockBackend() {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    switch (cmd) {
      case 'get_app_preferences':
        return Promise.resolve({
          default_provider: null,
          circuit_agent_pool_size: null,
          harness_defaults: {},
          confirm_before_quit: true,
        });
      case 'get_coordinator_status':
        return Promise.resolve({ enabled: false, has_token: false });
      case 'get_network_status':
        return Promise.resolve({ lan_exposure_enabled: false, port: 1992, tls_active: false, exposed_interfaces: [] });
      case 'list_providers':
        return Promise.resolve(PROVIDERS);
      case 'get_provider_accounts':
      case 'get_provider_pairings':
      case 'get_pairing_verifications':
      case 'get_keyed_first_class_catalog':
      case 'compatible_providers_for_harness':
      case 'list_device_sessions':
      case 'list_spawn_configurations':
      case 'get_launch_targets':
        return Promise.resolve([]);
      case 'get_probe_spawn_prompt_defaults':
        return Promise.resolve({ issue_template: 'ISSUE DEFAULT', pr_template: 'PR DEFAULT', review_policy: '' });
      default:
        return Promise.resolve({});
    }
  });
}

/** The label of the tab whose panel contains `element`. */
function owningTab(element: HTMLElement): string | undefined {
  const panel = element.closest('[role="tabpanel"]');
  const tabId = panel?.getAttribute('aria-labelledby');
  return tabId ? document.getElementById(tabId)?.textContent?.trim() : undefined;
}

describe('Settings dialog layout', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    mockBackend();
  });

  it('orders the tabs by how often people reach for them, with Autopilot and Prompts split out', async () => {
    render(<AppSettingsModal onClose={() => {}} />);
    await screen.findByRole('tab', { name: 'General' });
    expect(screen.getAllByRole('tab').map(tab => tab.textContent?.trim())).toEqual([
      'General',
      'Providers',
      'Launch Configurations',
      'Autopilot',
      'Prompts',
      'Remote Access',
      'Data & Diagnostics',
    ]);
  });

  describe('General', () => {
    it('is one labelled checkbox for quit confirmation, with no sentence beside it', async () => {
      render(<AppSettingsModal onClose={() => {}} />);
      const box = await screen.findByRole('checkbox', { name: 'Confirm before quitting' });
      expect(owningTab(box)).toBe('General');
      expect(screen.queryByText(/when agent sessions are active/i)).toBeNull();
    });

    it('no longer carries the Circuit pool size or the probe prompts', async () => {
      render(<AppSettingsModal onClose={() => {}} />);
      await screen.findByRole('checkbox', { name: 'Confirm before quitting' });
      expect(screen.queryByRole('spinbutton', { name: /circuit agent pool size/i })).toBeNull();
      expect(owningTab(screen.getByTestId('probe-prompt-card-issue'))).not.toBe('General');
    });
  });

  describe('Autopilot and Prompts', () => {
    it('puts the Circuit agent pool size on the Autopilot tab', async () => {
      render(<AppSettingsModal onClose={() => {}} />);
      await openSettingsPane('Autopilot');
      const input = await screen.findByRole('spinbutton', { name: /circuit agent pool size/i });
      expect(owningTab(input)).toBe('Autopilot');
    });

    it('flags the Autopilot tab, not General, while the pool size draft is unsaved', async () => {
      const user = userEvent.setup();
      render(<AppSettingsModal onClose={() => {}} />);
      await openSettingsPane('Autopilot');
      const input = await screen.findByRole('spinbutton', { name: /circuit agent pool size/i });
      await waitFor(() => expect((input as HTMLInputElement).disabled).toBe(false));
      await user.type(input, '4');
      expect(screen.getByTestId('settings-tab-dirty-autopilot')).toBeTruthy();
      expect(screen.queryByTestId('settings-tab-dirty-general')).toBeNull();
    });

    it('puts both probe spawn prompts on the Prompts tab', async () => {
      render(<AppSettingsModal onClose={() => {}} />);
      await openSettingsPane('Prompts');
      expect(owningTab(await screen.findByTestId('probe-prompt-card-issue'))).toBe('Prompts');
      expect(owningTab(screen.getByTestId('probe-prompt-card-pr'))).toBe('Prompts');
    });

    it('flags the Prompts tab while a prompt draft is unsaved', async () => {
      const user = userEvent.setup();
      render(<AppSettingsModal onClose={() => {}} />);
      await openSettingsPane('Prompts');
      const input = await screen.findByTestId('probe-prompt-input-issue');
      await waitFor(() => expect((input as HTMLTextAreaElement).disabled).toBe(false));
      await user.type(input, ' extra');
      expect(screen.getByTestId('settings-tab-dirty-prompts')).toBeTruthy();
      expect(screen.queryByTestId('settings-tab-dirty-general')).toBeNull();
    });
  });

  describe('Launch Configurations', () => {
    it('lists Spawn menu order first and keeps the saved configurations behind a toggle', async () => {
      render(<AppSettingsModal onClose={() => {}} />);
      await openSettingsPane('Launch Configurations');
      const panel = (await screen.findByRole('tabpanel', { name: 'Launch Configurations' }));
      const headings = within(panel).getAllByRole('heading', { level: 3 }).map(h => h.textContent);
      expect(headings[0]).toBe('Spawn menu order');
      expect(headings).toContain('Launch Configurations');

      const toggle = within(panel).getByRole('button', { name: /^(show|hide) launch configurations$/i });
      expect(toggle.getAttribute('aria-expanded')).toBe('false');
      expect(within(panel).queryByRole('button', { name: /new launch configuration/i })).toBeNull();

      fireEvent.click(toggle);
      expect(toggle.getAttribute('aria-expanded')).toBe('true');
      expect(await within(panel).findByRole('button', { name: /new launch configuration/i })).toBeTruthy();
    });

    it('does not offer Terminal an Agent Harness default card, and drops the effort boilerplate', async () => {
      render(<AppSettingsModal onClose={() => {}} />);
      await openSettingsPane('Launch Configurations');
      await screen.findByTestId('harness-default-claude');
      expect(screen.queryByTestId('harness-default-terminal')).toBeNull();
      expect(screen.queryByText(/availability depends on the selected model/i)).toBeNull();
      expect(screen.queryByText(/does not accept model, effort, or permission-mode overrides/i)).toBeNull();
    });
  });

  describe('Remote Access', () => {
    it('labels the exposure toggle once, with no subtitle or sentence beside the checkbox', async () => {
      render(<AppSettingsModal onClose={() => {}} />);
      await openSettingsPane(/remote access/i);
      const box = await screen.findByRole('checkbox', { name: 'Expose to LAN / VPN' });
      expect(owningTab(box)).toBe('Remote Access');
      expect(screen.queryByText(/self-signed tls; loopback stays plain http/i)).toBeNull();
      expect(screen.queryByText(/expose to lan \/ vpn over self-signed tls/i)).toBeNull();
    });

    it('labels the coordinator toggle once as well', async () => {
      render(<AppSettingsModal onClose={() => {}} />);
      await openSettingsPane(/remote access/i);
      await screen.findByRole('checkbox', { name: 'Enable coordinator read API' });
      expect(screen.queryByText(/read-only http view of node status/i)).toBeNull();
    });
  });
});
