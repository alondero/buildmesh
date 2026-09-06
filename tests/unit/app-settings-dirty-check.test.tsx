/**
 * Integration test for the issue #730 wiring: AccountCard / AddProviderForm
 * / HarnessCard each report their dirty state to AppSettingsModal, which feeds
 * the `<Modal dirty={...}>` prop. The Modal then intercepts backdrop-click
 * and Escape with an inline "Discard unsaved changes?" confirm.
 *
 * The unit tests for `<Modal>` already cover the primitive's contract (backdrop
 * intercepts, Escape intercepts, Keep-editing restores focus, Discard closes).
 * This file pins the *wiring* — that a typed draft in any of the three
 * children actually flips the Modal into dirty mode in the live app shell.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import { openSettingsPane } from '../utils/settings-panes';

function anthropicAccount() {
  return {
    id: 'anthropic',
    name: 'Anthropic / Claude',
    enabled: true,
    billing_mode: 'plan' as const,
    // Self-authenticating — no credential fields exposed. We change
    // claude_compatible on a custom account below for the credential
    // form half-typed test.
    claude_compatible: false,
    api_key: null,
  };
}

function customKeyedAccount() {
  return {
    id: 'deepseek',
    name: 'DeepSeek via Claude Code',
    enabled: true,
    billing_mode: 'pay_as_you_go' as const,
    claude_compatible: true,
    api_key: null,
  };
}

function mockBackend(opts: { accounts?: ReturnType<typeof anthropicAccount>[] } = {}) {
  const accounts = opts.accounts ?? [anthropicAccount()];
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    switch (cmd) {
      case 'get_app_preferences': return Promise.resolve({ default_provider: null, provider_pairings: [] });
      case 'list_providers': return Promise.resolve([]);
      case 'get_provider_accounts': return Promise.resolve(accounts);
      case 'get_keyed_first_class_catalog': return Promise.resolve([]);
      case 'get_provider_pairings': return Promise.resolve([]);
      case 'compatible_providers_for_harness': return Promise.resolve([]);
      case 'get_coordinator_status': return Promise.resolve({ enabled: false, has_token: false });
      case 'list_device_sessions': return Promise.resolve([]);
      case 'get_network_status': return Promise.resolve({ lan_exposure_enabled: false, port: 1992, tls_active: false, exposed_interfaces: [] });
      default: return Promise.resolve({});
    }
  });
}

/** The visible dimmer div is the wrapper's first child. Click it instead
 *  of the wrapper to exercise the real-DOM backdrop path. Issue #1292:
 *  the modal is now portaled to `document.body`, so we walk from the
 *  dialog role upward instead of from the render container — same
 *  structure (wrapper > dimmer + panel), same click target. */
function getBackdrop(): HTMLElement {
  const wrapper = screen.getByRole('dialog').parentElement as HTMLElement;
  return wrapper.firstElementChild as HTMLElement;
}

/** Open Add provider → Other / custom so the name+key form is dirty-trackable. */
async function openGenericAddForm(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole('button', { name: /^\+ add provider$/i }));
  await user.click(screen.getByRole('button', { name: /other \/ custom/i }));
}

describe('AppSettingsModal dirty-check wiring (issue #730)', () => {
  beforeEach(() => vi.mocked(invoke).mockReset());

  it('AddCustom: backdrop click shows the discard banner instead of closing', async () => {
    mockBackend();
    const onClose = vi.fn();
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={onClose} />);

    await screen.findByText('Anthropic / Claude');
    await openSettingsPane('Providers');
    await openGenericAddForm(user);
    await user.type(screen.getByLabelText(/custom provider name/i), 'Foo');

    fireEvent.click(getBackdrop());
    expect(onClose).not.toHaveBeenCalled();
    screen.getByTestId('modal-discard-banner');
  });

  it('AddCustom: Escape after typing shows the discard banner and does NOT close', async () => {
    mockBackend();
    const onClose = vi.fn();
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={onClose} />);

    await screen.findByText('Anthropic / Claude');
    await openSettingsPane('Providers');
    await openGenericAddForm(user);
    await user.type(screen.getByLabelText(/custom provider name/i), 'Foo');

    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClose).not.toHaveBeenCalled();
    screen.getByTestId('modal-discard-banner');
  });

  it('AddCustom: cancelling the form clears the dirty site so the modal is no longer dirty', async () => {
    // Regression for the code-review catch: a child→parent dirty report with
    // no unmount cleanup leaves the parent Set stuck. The Cancel button must
    // explicitly clear the site before unmounting.
    mockBackend();
    const onClose = vi.fn();
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={onClose} />);

    await screen.findByText('Anthropic / Claude');
    await openSettingsPane('Providers');
    await openGenericAddForm(user);
    await user.type(screen.getByLabelText(/custom provider name/i), 'Foo');

    // Verify dirty before cancel.
    fireEvent.click(getBackdrop());
    expect(onClose).not.toHaveBeenCalled();
    fireEvent.click(screen.getByTestId('modal-discard-cancel'));

    // Cancel the form (no save).
    await user.click(screen.getByRole('button', { name: /^cancel$/i }));

    // After cancel, backdrop should close the modal cleanly (not show the banner).
    onClose.mockClear();
    fireEvent.click(getBackdrop());
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('AccountCard: typing in the API key of a custom Claude-compatible account makes the modal dirty', async () => {
    // The Anthropic account is self-authenticating, so we mount a custom
    // claude_compatible account that exposes the credential fields. The
    // user types in the API key and hits Escape — the modal must NOT close
    // and the discard banner must appear.
    mockBackend({ accounts: [customKeyedAccount()] });
    const onClose = vi.fn();
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={onClose} />);

    await screen.findByText('DeepSeek via Claude Code');
    await openSettingsPane('Providers');
    // Open the credentials editor.
    await user.click(screen.getByRole('button', { name: /edit credentials/i }));
    // Type a half-typed API key.
    await user.type(screen.getByLabelText(/deepseek via claude code api key/i), 'sk-half-typed');

    // The modal is now dirty: Escape must surface the banner.
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(onClose).not.toHaveBeenCalled();
    screen.getByTestId('modal-discard-banner');
  });
});
