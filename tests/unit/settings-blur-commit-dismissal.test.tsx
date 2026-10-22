/**
 * Issue #2458 — "save on blur" versus the modal's discard prompt.
 *
 * A Settings field that commits its draft when focus leaves it collides
 * with the unsaved-changes guard, because a dismissal gesture is a focus
 * move:
 *
 *   - Escape makes the modal focus the banner's "Keep editing" button,
 *   - the header × and the backdrop move focus on `mousedown`, before the
 *     `click` that starts the dismissal even runs.
 *
 * So the blur fired *while the user was trying to abandon the edit*, the
 * commit persisted the draft, the dirty signal cleared, and the discard
 * prompt could be skipped altogether (× / backdrop) or dismissed again on
 * the next frame (Escape). These tests pin the fixed behaviour: the prompt
 * appears, the draft stays uncommitted, and Discard really discards.
 *
 * The browser's ordering is reproduced explicitly (mousedown → blur →
 * click) rather than through a synthetic `click` alone, because a bare
 * `click` does not move focus and so does not fire the blur that the whole
 * bug turns on.
 */
import { describe, it, expect, vi, beforeEach } from 'vitest';
import { render, screen, waitFor, fireEvent } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { invoke } from '@tauri-apps/api/core';
import { AppSettingsModal } from '../../src/components/AppSettings/AppSettingsModal';
import { openSettingsPane } from '../utils/settings-panes';

vi.mock('@tauri-apps/api/app', () => ({ getVersion: vi.fn().mockResolvedValue('1.5.0') }));

function mockBackend(worktreeDir: string | null = null) {
  const calls: Record<string, unknown[]> = {};
  vi.mocked(invoke).mockImplementation((cmd: string, args?: Record<string, unknown>) => {
    calls[cmd] = [...(calls[cmd] ?? []), args];
    switch (cmd) {
      case 'get_app_preferences':
        return Promise.resolve({
          default_provider: null,
          minimax_api_key: null,
          circuit_agent_pool_size: null,
          worktree_directory: worktreeDir,
          harness_defaults: {},
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

/** The visible dimmer div is the portal wrapper's first child. Issue #1292
 *  moved the modal to `document.body`, so walk up from the dialog role. */
function getBackdrop(): HTMLElement {
  const wrapper = screen.getByRole('dialog').parentElement as HTMLElement;
  return wrapper.firstElementChild as HTMLElement;
}

const dirInput = async () =>
  (await screen.findByRole('textbox', { name: /worktree directory/i })) as HTMLInputElement;

const closeButton = () => screen.getByRole('button', { name: /close settings/i });

/**
 * Press the header × the way a browser does: `mousedown` moves focus to
 * the button (which blurs the field — the blur that used to commit the
 * draft), and the `click` that follows starts the dismissal.
 */
async function pressCloseButton(user: ReturnType<typeof userEvent.setup>) {
  const button = closeButton();
  await user.click(button);
}

/**
 * Click the backdrop the way a browser does: `mousedown` on the dimmer
 * drops focus to the body (again blurring the field), and the `click`
 * that follows starts the dismissal.
 */
async function clickBackdrop(user: ReturnType<typeof userEvent.setup>) {
  await user.click(getBackdrop());
}

describe('Settings — blur-to-save vs. the discard prompt (issue #2458)', () => {
  beforeEach(() => vi.mocked(invoke).mockReset());

  it('Escape on a dirty draft opens the prompt and does not persist the draft', async () => {
    const calls = mockBackend(null);
    const onClose = vi.fn();
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={onClose} />);

    const input = await dirInput();
    await waitFor(() => expect(input.disabled).toBe(false));
    await user.type(input, 'uncommitted-test-path');
    screen.getByTestId('settings-tab-dirty-general');

    fireEvent.keyDown(document, { key: 'Escape' });

    expect(onClose).not.toHaveBeenCalled();
    screen.getByTestId('modal-discard-banner');
    // The draft was not persisted — the prompt is still asking about it.
    expect(calls['set_app_worktree_directory']).toBeUndefined();
    screen.getByTestId('settings-tab-dirty-general');
  });

  it('the header × on a dirty draft opens the prompt and does not persist the draft', async () => {
    const calls = mockBackend(null);
    const onClose = vi.fn();
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={onClose} />);

    const input = await dirInput();
    await waitFor(() => expect(input.disabled).toBe(false));
    await user.type(input, 'uncommitted-test-path');

    await pressCloseButton(user);

    expect(onClose).not.toHaveBeenCalled();
    screen.getByTestId('modal-discard-banner');
    expect(calls['set_app_worktree_directory']).toBeUndefined();
    screen.getByTestId('settings-tab-dirty-general');
  });

  it('a backdrop click on a dirty draft opens the prompt and does not persist the draft', async () => {
    const calls = mockBackend(null);
    const onClose = vi.fn();
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={onClose} />);

    const input = await dirInput();
    await waitFor(() => expect(input.disabled).toBe(false));
    await user.type(input, 'uncommitted-test-path');

    await clickBackdrop(user);

    expect(onClose).not.toHaveBeenCalled();
    screen.getByTestId('modal-discard-banner');
    expect(calls['set_app_worktree_directory']).toBeUndefined();
    screen.getByTestId('settings-tab-dirty-general');
  });

  it('Discard closes the modal without persisting the draft', async () => {
    const calls = mockBackend(null);
    const onClose = vi.fn();
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={onClose} />);

    const input = await dirInput();
    await waitFor(() => expect(input.disabled).toBe(false));
    await user.type(input, 'uncommitted-test-path');

    fireEvent.keyDown(document, { key: 'Escape' });
    screen.getByTestId('modal-discard-banner');
    await user.click(screen.getByTestId('modal-discard-confirm'));

    expect(onClose).toHaveBeenCalledTimes(1);
    expect(calls['set_app_worktree_directory']).toBeUndefined();
  });

  it('Keep editing keeps the draft dirty, unpersisted, and returns focus to the field', async () => {
    const calls = mockBackend(null);
    const onClose = vi.fn();
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={onClose} />);

    const input = await dirInput();
    await waitFor(() => expect(input.disabled).toBe(false));
    await user.type(input, 'uncommitted-test-path');

    fireEvent.keyDown(document, { key: 'Escape' });
    screen.getByTestId('modal-discard-banner');
    await user.click(screen.getByTestId('modal-discard-cancel'));

    expect(onClose).not.toHaveBeenCalled();
    expect(screen.queryByTestId('modal-discard-banner')).toBeNull();
    // The draft survived the prompt untouched.
    expect(input.value).toBe('uncommitted-test-path');
    expect(calls['set_app_worktree_directory']).toBeUndefined();
    screen.getByTestId('settings-tab-dirty-general');
    // Focus went back to the field the user was editing.
    await waitFor(() => expect(document.activeElement).toBe(input));
  });

  it('the Circuit pool-size field follows the same rule', async () => {
    const calls = mockBackend(null);
    const onClose = vi.fn();
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={onClose} />);

    await openSettingsPane('Autopilot');
    const input = (await screen.findByRole('spinbutton', {
      name: /Circuit agent pool size/i,
    })) as HTMLInputElement;
    await waitFor(() => expect(input.disabled).toBe(false));
    await user.type(input, '8');
    screen.getByTestId('settings-tab-dirty-autopilot');

    fireEvent.keyDown(document, { key: 'Escape' });

    expect(onClose).not.toHaveBeenCalled();
    screen.getByTestId('modal-discard-banner');
    expect(calls['set_app_circuit_agent_pool_size']).toBeUndefined();
    screen.getByTestId('settings-tab-dirty-autopilot');
  });

  it('still saves on blur when no dismissal is in flight', async () => {
    const calls = mockBackend(null);
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);

    const input = await dirInput();
    await waitFor(() => expect(input.disabled).toBe(false));
    await user.type(input, 'my-wt');

    // Focus moves to another control inside the modal — an ordinary
    // interaction, not a dismissal. The blur-commit still applies.
    fireEvent.blur(input, { relatedTarget: closeButton() });

    await waitFor(() => expect(calls['set_app_worktree_directory']).toBeTruthy());
    expect(calls['set_app_worktree_directory']![0]).toEqual({ directory: 'my-wt' });
  });

  it('saves on blur when the user switches Settings tabs', async () => {
    // The tab switch is a focus move inside the modal, not a dismissal, so
    // the documented blur-to-save contract applies: the value is persisted
    // and the pane stops being dirty. A dismissal attempt defers the
    // commit instead.
    const calls = mockBackend(null);
    const user = userEvent.setup();
    render(<AppSettingsModal onClose={() => {}} />);

    const input = await dirInput();
    await waitFor(() => expect(input.disabled).toBe(false));
    await user.type(input, 'my-wt');
    screen.getByTestId('settings-tab-dirty-general');

    // A real click: `mousedown` moves focus to the tab, which blurs the
    // field — the blur the blur-to-save contract hangs on.
    await user.click(screen.getByRole('tab', { name: 'Autopilot' }));

    await waitFor(() => expect(calls['set_app_worktree_directory']).toBeTruthy());
    expect(calls['set_app_worktree_directory']![0]).toEqual({ directory: 'my-wt' });
    expect(screen.queryByTestId('settings-tab-dirty-general')).toBeNull();
  });
});
