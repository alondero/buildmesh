import { beforeEach, describe, expect, it, vi } from 'vitest';
import { render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';

beforeEach(() => {
  vi.mocked(invoke).mockImplementation(async (command: string) => {
    switch (command) {
      case 'get_app_preferences': return { default_provider: null, provider_pairings: [] };
      case 'get_coordinator_status': return { enabled: false, has_token: false };
      case 'get_network_status': return { lan_exposure_enabled: false, exposed_interfaces: [] };
      default: return [];
    }
  });
});

describe('Settings keyboard navigation', () => {
  it('automatically selects wrapping vertical tabs and links each panel', async () => {
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={vi.fn()} />);
    const tabs = within(screen.getByRole('tablist')).getAllByRole('tab');
    // Derived from the tab count rather than hard-coded: this asserts that
    // `End` lands on the *last* tab, which is the property that matters.
    // Pinning an index instead re-breaks on every tab the Settings modal
    // gains (issue #1537 added "Data & Diagnostics").
    const last = tabs.length - 1;
    expect(document.activeElement).toBe(tabs[0]);
    for (const [key, index] of [['ArrowUp', last], ['ArrowDown', 0], ['End', last], ['Home', 0], ['ArrowDown', 1]] as const) {
      await user.keyboard(`{${key}}`);
      expect(document.activeElement).toBe(tabs[index]);
      expect(tabs[index].getAttribute('aria-selected')).toBe('true');
      expect(tabs.filter(tab => tab.tabIndex === 0)).toEqual([tabs[index]]);
      const panel = screen.getByRole('tabpanel');
      expect(panel.id).toBe(tabs[index].getAttribute('aria-controls'));
      expect(panel.getAttribute('aria-labelledby')).toBe(tabs[index].id);
    }
    await user.keyboard('{ArrowRight}{ArrowLeft}');
    expect(document.activeElement).toBe(tabs[1]);
    await user.tab();
    expect(document.activeElement).toBe(screen.getByRole('tabpanel', { name: 'Providers' }));
  });

  it('preserves drafts across tabs and returns from the dirty banner to the selected pane', async () => {
    const user = userEvent.setup();
    const onClose = vi.fn();
    render(<AppSettingsModal onClose={onClose} />);
    const providers = screen.getByRole('tab', { name: 'Providers' });
    await user.click(providers);
    await user.click(await screen.findByRole('button', { name: /^\+ add provider$/i }));
    await user.click(screen.getByRole('button', { name: /other \/ custom/i }));
    const field = screen.getByLabelText(/custom provider name/i);
    await user.type(field, 'Draft provider');
    await user.click(screen.getByRole('tab', { name: 'General' }));
    await user.keyboard('{ArrowDown}');
    expect((field as HTMLInputElement).value).toBe('Draft provider');
    await user.click(field);
    await user.keyboard('{Escape}');
    expect(document.activeElement).toBe(screen.getByTestId('modal-discard-cancel'));
    await user.tab();
    expect(document.activeElement).toBe(screen.getByTestId('modal-discard-confirm'));
    await user.tab();
    expect(document.activeElement).toBe(screen.getByTestId('modal-discard-cancel'));
    await user.keyboard('{Enter}');
    await vi.waitFor(() => expect(document.activeElement).toBe(field));
    expect(onClose).not.toHaveBeenCalled();
  });
});
