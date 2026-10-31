import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { CodexLoginsSection, SIGN_IN_POLL_MS, type CodexLoginsApi } from '../../src/components/AppSettings/CodexLoginsSection';
import { LaunchConfigurationEditor } from '../../src/components/Providers/LaunchConfigurationEditor';
import { UsagePanel } from '../../src/components/AppSettings/UsageRender';
import { usageCountLabel } from '../../src/components/Probe/UsageTab';
import { __resetHarnessLoginsForTests, publishHarnessLogins } from '../../src/hooks/useHarnessLogins';
import type { HarnessLoginStatus } from '../../src/types/generated/HarnessLoginStatus';
import type { LaunchTarget } from '../../src/types/generated/LaunchTarget';
import type { SpawnConfiguration } from '../../src/types/generated/SpawnConfiguration';
import type { ProviderAccount, ProviderMeters } from '../../src/lib/tauri';

vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn().mockResolvedValue(undefined) }));

const login = (id: string, label: string, extra: Partial<HarnessLoginStatus> = {}): HarnessLoginStatus => ({
  id, harness_id: 'codex', label, home: `C:/u/.codex-accounts/${id}`, is_default: id === 'default',
  signed_in: true, email: null, plan: null, same_account_as: null, sign_in: null, ...extra,
});
const DEFAULT = login('default', 'Default', { home: 'C:/u/.codex', email: 'me@home.example', plan: 'pro' });
const WORK = login('work', 'Work', { email: 'me@work.example', plan: 'pro' });

beforeEach(() => __resetHarnessLoginsForTests());
afterEach(() => { cleanup(); vi.useRealTimers(); });

function fakeApi(list: HarnessLoginStatus[][]): CodexLoginsApi & { [K in keyof CodexLoginsApi]: ReturnType<typeof vi.fn> } {
  let call = 0;
  return {
    list: vi.fn(async () => list[Math.min(call++, list.length - 1)]),
    add: vi.fn().mockResolvedValue(undefined),
    rename: vi.fn().mockResolvedValue(undefined),
    remove: vi.fn().mockResolvedValue(undefined),
    startSignIn: vi.fn().mockResolvedValue(undefined),
    cancelSignIn: vi.fn().mockResolvedValue(undefined),
  };
}

describe('Settings › Launch Configurations › Codex logins', () => {
  it('lists each login with its account, plan and folder', async () => {
    render(<CodexLoginsSection api={fakeApi([[DEFAULT, WORK, login('spare', 'Spare', { signed_in: false })]])} />);
    const list = await screen.findByRole('list', { name: 'Codex logins' });
    const rows = within(list).getAllByRole('listitem');
    expect(rows[0].textContent).toContain('me@home.example · pro');
    expect(rows[1].textContent).toContain('C:/u/.codex-accounts/work');
    expect(rows[2].textContent).toContain('Not signed in');
    expect(within(rows[2]).getByRole('button', { name: 'Sign in' })).toBeTruthy();
    // The default login has no folder of its own to rename or remove.
    expect(within(rows[0]).queryByRole('button', { name: 'Remove' })).toBeNull();
    expect(within(rows[0]).getByRole('button', { name: 'Sign in again' })).toBeTruthy();
  });

  it('loads the list under StrictMode, which remounts effects in development', async () => {
    render(<StrictMode><CodexLoginsSection api={fakeApi([[DEFAULT, WORK]])} /></StrictMode>);
    expect(await screen.findByRole('list', { name: 'Codex logins' })).toBeTruthy();
  });

  it('adds a named login and re-reads the list', async () => {
    const api = fakeApi([[DEFAULT], [DEFAULT, WORK]]);
    render(<CodexLoginsSection api={api} />);
    await screen.findByRole('list', { name: 'Codex logins' });
    await userEvent.type(screen.getByLabelText('New login name'), 'Work');
    await userEvent.click(screen.getByRole('button', { name: 'Add login' }));
    expect(api.add).toHaveBeenCalledWith('Work');
    await screen.findByText('me@work.example · pro');
    expect((screen.getByLabelText('New login name') as HTMLInputElement).value).toBe('');
  });

  it('renames and removes an additional login after confirmation', async () => {
    const api = fakeApi([[DEFAULT, WORK]]);
    render(<CodexLoginsSection api={api} />);
    const list = await screen.findByRole('list', { name: 'Codex logins' });
    const work = within(list).getAllByRole('listitem')[1];
    await userEvent.click(within(work).getByRole('button', { name: 'Rename' }));
    const input = within(work).getByLabelText('New name for Work');
    await userEvent.clear(input);
    await userEvent.type(input, 'Client');
    await userEvent.click(within(work).getByRole('button', { name: 'Save' }));
    expect(api.rename).toHaveBeenCalledWith('work', 'Client');

    await userEvent.click(within(work).getByRole('button', { name: 'Remove' }));
    const confirm = within(work).getByRole('group', { name: 'Remove Work' });
    expect(confirm.textContent).toContain('Its folder stays at C:/u/.codex-accounts/work');
    expect(api.remove).not.toHaveBeenCalled();
    await userEvent.click(within(confirm).getByRole('button', { name: 'Remove login' }));
    expect(api.remove).toHaveBeenCalledWith('work');
  });

  it('shows the sign-in link while codex login waits, polls, and settles once signed in', async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    const waiting = { ...WORK, signed_in: false, email: null, sign_in: { running: true, url: 'https://auth.openai.com/oauth/authorize?x=1', error: null } };
    const api = fakeApi([[DEFAULT, waiting], [DEFAULT, waiting], [DEFAULT, WORK]]);
    render(<CodexLoginsSection api={api} />);
    const status = await screen.findByRole('status');
    expect(status.textContent).toContain('private window');
    expect(within(status).getByText('https://auth.openai.com/oauth/authorize?x=1')).toBeTruthy();
    // A second sign-in waits for this one: codex login holds one local port.
    expect(screen.getByRole('button', { name: 'Sign in again' }).hasAttribute('disabled')).toBe(true);
    await act(async () => { await vi.advanceTimersByTimeAsync(SIGN_IN_POLL_MS * 2); });
    await waitFor(() => expect(screen.queryByRole('status')).toBeNull());
    expect(screen.getByText('me@work.example · pro')).toBeTruthy();
    const calls = api.list.mock.calls.length;
    await act(async () => { await vi.advanceTimersByTimeAsync(SIGN_IN_POLL_MS * 2); });
    expect(api.list.mock.calls.length).toBe(calls);
  });

  it('warns when two logins are signed in to the same ChatGPT account', async () => {
    render(<CodexLoginsSection api={fakeApi([[DEFAULT, { ...WORK, same_account_as: 'Default' }]])} />);
    expect((await screen.findByRole('note')).textContent).toContain('same ChatGPT account as Default');
  });

  it('reports a failed sign-in start', async () => {
    const api = fakeApi([[DEFAULT, WORK]]);
    api.startSignIn.mockRejectedValueOnce('Codex is not installed or not on PATH');
    render(<CodexLoginsSection api={api} />);
    const list = await screen.findByRole('list', { name: 'Codex logins' });
    await userEvent.click(within(within(list).getAllByRole('listitem')[1]).getByRole('button', { name: 'Sign in again' }));
    expect((await screen.findByRole('alert')).textContent).toContain('not on PATH');
  });
});

describe('pinning a login on a Launch Configuration', () => {
  const codex: LaunchTarget = {
    id: 'codex', harness_id: 'codex', harness_name: 'Codex', provider_name: 'OpenAI',
    models: [], efforts: [], route_attached: false, manual_model: true, supports_model: true, supports_extra_args: true,
  };
  const value: SpawnConfiguration = { id: 'launch/codex', name: 'Codex', spawn_option_id: 'codex', model: null, effort: null, extra_args: null };

  it('saves the pinned login with the configuration', async () => {
    publishHarnessLogins([DEFAULT, WORK]);
    const save = vi.fn().mockResolvedValue(undefined);
    render(<LaunchConfigurationEditor value={value} targets={[codex]} onSave={save} onCancel={vi.fn()} />);
    const select = screen.getByLabelText('Login') as HTMLSelectElement;
    expect([...select.options].map((option) => option.textContent)).toEqual([
      'Login picked in the spawn menu', 'Work (me@work.example)',
    ]);
    fireEvent.change(select, { target: { value: 'work' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(save).toHaveBeenCalledWith({ ...value, login_id: 'work' }, undefined, expect.any(Function)));
  });

  it('offers no login choice with a single login', () => {
    publishHarnessLogins([DEFAULT]);
    render(<LaunchConfigurationEditor value={value} targets={[codex]} onSave={vi.fn()} onCancel={vi.fn()} />);
    expect(screen.queryByLabelText('Login')).toBeNull();
  });

  it('offers no login choice on a Provider Route', () => {
    publishHarnessLogins([DEFAULT, WORK]);
    const routed: LaunchTarget = { ...codex, id: 'codex:minimax', provider_name: 'MiniMax', route_attached: true,
      route: { harness_id: 'codex', provider_id: 'minimax', surface: 'openai', base_url: 'https://api.minimax.io/v1',
        model_tiers: { default: 'MiniMax-M3', opus: null, fable: null, sonnet: null, haiku: null, small_fast: null } } };
    render(<LaunchConfigurationEditor value={{ ...value, spawn_option_id: 'codex:minimax' }} targets={[routed]} onSave={vi.fn()} onCancel={vi.fn()} />);
    expect(screen.queryByLabelText('Login')).toBeNull();
  });
});

describe('usage per login', () => {
  const codexAccount: ProviderAccount = {
    id: 'codex', name: 'Codex', enabled: true, billing_mode: 'plan', claude_compatible: false, api_key: null,
  };
  const meter = (login?: ProviderMeters['login']): ProviderMeters => ({
    provider: 'codex', usageTracked: true, login,
    usage: { provider: 'codex', loggedIn: true, windows: [{ label: '5-hour', usedPercent: 12, resetsAt: null }], balance: null, meters: [], detail: null, error: null },
  });

  it('names the login and its account on each Codex panel', () => {
    render(<>
      <UsagePanel account={codexAccount} meter={meter({ id: 'default', label: 'Default' })} onRefresh={vi.fn()} />
      <UsagePanel account={codexAccount} meter={meter({ id: 'work', label: 'Work', email: 'me@work.example' })} onRefresh={vi.fn()} />
    </>);
    expect(screen.getByTestId('usage-panel-codex').textContent).toContain('Default');
    const work = screen.getByTestId('usage-panel-codex-work');
    expect(within(work).getByTestId('usage-panel-login').textContent).toBe('Work · me@work.example');
    expect(within(work).getByRole('button', { name: 'Refresh usage for Codex (Work)' })).toBeTruthy();
  });

  it('points a signed-out additional login at its sign-in', () => {
    const signedOut = meter({ id: 'work', label: 'Work' });
    signedOut.usage = { ...signedOut.usage!, loggedIn: false };
    render(<UsagePanel account={codexAccount} meter={signedOut} />);
    expect(screen.getByText('Sign in to Work in Settings › Launch Configurations')).toBeTruthy();
  });

  it('counts providers, and meters only when logins add more', () => {
    expect(usageCountLabel([meter()])).toBe('1 provider tracked');
    expect(usageCountLabel([
      meter({ id: 'default', label: 'Default' }), meter({ id: 'work', label: 'Work' }),
      { ...meter(), provider: 'minimax' },
    ])).toBe('2 providers tracked (3 meters)');
  });
});
