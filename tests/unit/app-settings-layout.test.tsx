/** Settings draft retention, dirty indicators and navigation affordances. */
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
        return Promise.resolve([]);
      case 'get_launch_targets':
        return Promise.resolve([
          {
            id: 'claude', harness_id: 'claude', harness_name: 'Claude Code', provider_name: 'Native authentication',
            models: [], efforts: [], route_attached: false, manual_model: false, supports_model: false, supports_extra_args: true,
          },
        ]);
      case 'get_probe_spawn_prompt_defaults':
        return Promise.resolve({ issue_template: 'ISSUE DEFAULT', pr_template: 'PR DEFAULT', review_policy: '' });
      default:
        return Promise.resolve({});
    }
  });
}

describe('Settings dialog layout', () => {
  beforeEach(() => {
    vi.mocked(invoke).mockReset();
    mockBackend();
  });

  describe('Autopilot and Prompts', () => {
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

    it('keeps an open editor draft and the unsaved dot while the list is collapsed', async () => {
      // Collapsing must hide the list, not unmount it: the editor holds the
      // draft and reports the dirty site (issue #730). Unmounting would drop a
      // half-typed configuration while the modal still reads dirty.
      render(<AppSettingsModal onClose={() => {}} />);
      await openSettingsPane('Launch Configurations');
      const panel = await screen.findByRole('tabpanel', { name: 'Launch Configurations' });
      const toggle = within(panel).getByRole('button', { name: /^(show|hide) launch configurations$/i });
      fireEvent.click(toggle);
      const add = await within(panel).findByRole('button', { name: 'New Launch Configuration' });
      await waitFor(() => expect((add as HTMLButtonElement).disabled).toBe(false));
      fireEvent.click(add);
      fireEvent.change(within(panel).getByLabelText('Name'), { target: { value: 'Half typed' } });
      expect(screen.getByTestId('settings-tab-dirty-harnesses')).toBeTruthy();

      fireEvent.click(toggle);
      expect(toggle.getAttribute('aria-expanded')).toBe('false');
      expect(screen.getByTestId('settings-tab-dirty-harnesses')).toBeTruthy();

      fireEvent.click(toggle);
      expect((within(panel).getByLabelText('Name') as HTMLInputElement).value).toBe('Half typed');
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

  describe('Providers help text', () => {
    // The Harnesses tab is now Launch Configurations, so help that sends people
    // "under Harnesses" points at a tab that does not exist.
    it('sends the Accounts section and the add-provider form to Launch Configurations', async () => {
      const user = userEvent.setup();
      render(<AppSettingsModal onClose={() => {}} />);
      await openSettingsPane('Providers');

      await user.hover(await screen.findByRole('button', { name: 'About Accounts' }));
      const tip = await screen.findByRole('tooltip');
      expect(tip.textContent).toMatch(/attach a provider under Launch Configurations/);
      expect(tip.textContent).not.toMatch(/Harnesses/);

      const add = await screen.findByRole('button', { name: '+ Add provider' });
      await waitFor(() => expect((add as HTMLButtonElement).disabled).toBe(false));
      await user.click(add);
      const form = await screen.findByTestId('add-provider-form');
      expect(form.textContent).toMatch(/attach it under Launch Configurations/);
      expect(form.textContent).not.toMatch(/Harnesses/);

      await user.click(within(form).getByRole('button', { name: /other \/ custom/i }));
      expect(form.textContent).toMatch(/Attach under Launch Configurations to set base URL/);
      expect(form.textContent).not.toMatch(/Harnesses/);
    });
  });
});
