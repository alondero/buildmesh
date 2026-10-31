import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { GroupedProviderMenu } from '../../src/components/Providers/GroupedProviderMenu';
import { offeredUnderLogin } from '../../src/components/Providers/SpawnConfigurationMenu';
import { placeFlyout } from '../../src/components/Providers/flyoutPlacement';
import { __resetHarnessLoginsForTests } from '../../src/hooks/useHarnessLogins';
import type { SpawnOption } from '../../src/lib/groups';
import * as api from '../../src/lib/tauri/provider';
import type { HarnessLoginStatus } from '../../src/types/generated/HarnessLoginStatus';
import type { SpawnConfiguration } from '../../src/types/generated/SpawnConfiguration';
import { HARNESS_CAPABILITIES } from '../../src/types/generated/HarnessCapabilitiesTable';

vi.mock('../../src/lib/tauri/provider', () => ({
  listSpawnConfigurations: vi.fn(),
  listHarnessLogins: vi.fn(),
  verifyLaunchConfiguration: vi.fn(),
  getLaunchTargets: vi.fn().mockResolvedValue([]),
  saveSpawnConfiguration: vi.fn(),
  deleteSpawnConfiguration: vi.fn(),
  listProviders: vi.fn(),
}));

const login = (id: string, label: string, extra: Partial<HarnessLoginStatus> = {}): HarnessLoginStatus => ({
  id, harness_id: 'codex', label, home: `C:/u/.codex-accounts/${id}`, is_default: id === 'default',
  signed_in: true, email: null, plan: null, same_account_as: null, sign_in: null, ...extra,
});
const DEFAULT = login('default', 'Default', { home: 'C:/u/.codex', email: 'me@home.example' });
const WORK = login('work', 'Work', { email: 'me@work.example' });

const codex: SpawnOption = {
  id: 'codex', label: 'Codex', icon: 'X', color: 'bg-gray-500', harness_id: 'codex',
  provider_id: null, is_proxied: false, group_key: 'codex', capabilities: HARNESS_CAPABILITIES.codex,
};
const configuration = (id: string, name: string, extra: Partial<SpawnConfiguration> = {}): SpawnConfiguration => ({
  id, name, spawn_option_id: 'codex', model: null, effort: null, extra_args: null, ...extra,
});

beforeEach(() => {
  __resetHarnessLoginsForTests();
  vi.mocked(api.listHarnessLogins).mockReset().mockResolvedValue([DEFAULT, WORK]);
  vi.mocked(api.listSpawnConfigurations).mockReset().mockResolvedValue([
    configuration('launch/any', 'Any login'),
    configuration('launch/work-only', 'Work only', { login_id: 'work' }),
    configuration('launch/other-only', 'Other only', { login_id: 'other' }),
  ]);
});
afterEach(() => cleanup());

async function openLogins() {
  const toggle = await screen.findByRole('button', { name: 'Codex logins' });
  await userEvent.click(toggle);
  return screen.findByRole('menu', { name: 'Codex logins' });
}

describe('the Codex login submenu', () => {
  it('lists every login between the harness row and its configurations', async () => {
    render(<GroupedProviderMenu providers={[codex]} onSelect={vi.fn()} loginsEnabled />);
    const menu = await openLogins();
    const rows = within(menu).getAllByRole('menuitem');
    expect(rows.map((row) => row.textContent)).toEqual([
      'Defaultme@home.example',
      'Workme@work.example',
    ]);
    // The configuration submenu waits for a login to be chosen.
    expect(screen.queryByRole('menu', { name: 'Codex configurations' })).toBeNull();
  });

  it('spawns the defaults with the picked login', async () => {
    const onSelect = vi.fn();
    render(<GroupedProviderMenu providers={[codex]} onSelect={onSelect} loginsEnabled />);
    const menu = await openLogins();
    await userEvent.click(within(menu).getByRole('menuitem', { name: /^Work/ }));
    expect(onSelect).toHaveBeenCalledWith('codex', false, undefined, 'work');
  });

  it("offers unpinned configurations and the login's own pins, and launches them with that login", async () => {
    const onSelect = vi.fn();
    render(<GroupedProviderMenu providers={[codex]} onSelect={onSelect} loginsEnabled />);
    const menu = await openLogins();
    await userEvent.click(within(menu).getByRole('button', { name: 'Work configurations' }));
    const configurations = await screen.findByRole('menu', { name: 'Codex Work configurations' });
    await within(configurations).findByRole('menuitem', { name: 'Any login' });
    expect(within(configurations).getByRole('menuitem', { name: 'Work only' })).toBeTruthy();
    expect(within(configurations).queryByRole('menuitem', { name: 'Other only' })).toBeNull();

    await userEvent.click(within(configurations).getByRole('menuitem', { name: 'Work only' }));
    expect(onSelect).toHaveBeenCalledWith('codex', false, 'launch/work-only', 'work');
    await userEvent.click(within(configurations).getByRole('menuitem', { name: 'Spawn with defaults' }));
    expect(onSelect).toHaveBeenLastCalledWith('codex', false, undefined, 'work');
  });

  it('hides a configuration pinned to another login from the Default login', async () => {
    render(<GroupedProviderMenu providers={[codex]} onSelect={vi.fn()} loginsEnabled />);
    const menu = await openLogins();
    await userEvent.click(within(menu).getByRole('button', { name: 'Default configurations' }));
    const configurations = await screen.findByRole('menu', { name: 'Codex Default configurations' });
    await within(configurations).findByRole('menuitem', { name: 'Any login' });
    expect(within(configurations).queryByRole('menuitem', { name: 'Work only' })).toBeNull();
  });

  it('offers a Provider Route configuration only under the Default login, which it ignores', async () => {
    vi.mocked(api.listSpawnConfigurations).mockResolvedValue([
      configuration('launch/any', 'Any login'),
      configuration('launch/minimax', 'MiniMax route', { spawn_option_id: 'codex:minimax', provider_route_id: 'minimax' }),
    ]);
    render(<GroupedProviderMenu providers={[codex]} onSelect={vi.fn()} loginsEnabled />);
    const menu = await openLogins();
    await userEvent.click(within(menu).getByRole('button', { name: 'Work configurations' }));
    const work = await screen.findByRole('menu', { name: 'Codex Work configurations' });
    await within(work).findByRole('menuitem', { name: 'Any login' });
    expect(within(work).queryByRole('menuitem', { name: 'MiniMax route' })).toBeNull();
    await userEvent.click(within(menu).getByRole('button', { name: 'Default configurations' }));
    const fallback = await screen.findByRole('menu', { name: 'Codex Default configurations' });
    expect(await within(fallback).findByRole('menuitem', { name: 'MiniMax route' })).toBeTruthy();
  });

  it('keeps an additional login that is not signed in out of reach', async () => {
    vi.mocked(api.listHarnessLogins).mockResolvedValue([DEFAULT, login('spare', 'Spare', { signed_in: false })]);
    const onSelect = vi.fn();
    render(<GroupedProviderMenu providers={[codex]} onSelect={onSelect} loginsEnabled />);
    const menu = await openLogins();
    const spare = within(menu).getByRole('menuitem', { name: /^Spare/ });
    expect(spare.getAttribute('aria-disabled')).toBe('true');
    expect(spare.textContent).toContain('Not signed in');
    expect(spare.getAttribute('title')).toContain('Settings › Launch Configurations');
    await userEvent.click(spare);
    expect(onSelect).not.toHaveBeenCalled();
    expect(within(menu).queryByRole('button', { name: 'Spare configurations' })).toBeNull();
  });

  it('flags a login signed in to the same ChatGPT account as another', async () => {
    vi.mocked(api.listHarnessLogins).mockResolvedValue([DEFAULT, { ...WORK, same_account_as: 'Default' }]);
    render(<GroupedProviderMenu providers={[codex]} onSelect={vi.fn()} loginsEnabled />);
    const menu = await openLogins();
    expect(within(menu).getByRole('menuitem', { name: /^Work/ }).textContent).toContain('Same ChatGPT account as Default');
  });

  it('walks in and out with the arrow keys', async () => {
    render(<GroupedProviderMenu providers={[codex]} onSelect={vi.fn()} loginsEnabled />);
    await screen.findByRole('button', { name: 'Codex logins' });
    const harnessRow = screen.getByRole('menuitem', { name: 'Codex' });
    harnessRow.focus();
    await userEvent.keyboard('{ArrowRight}');
    const menu = await screen.findByRole('menu', { name: 'Codex logins' });
    await waitFor(() => expect(document.activeElement?.getAttribute('data-login-id')).toBe('default'));
    await userEvent.keyboard('{ArrowDown}');
    expect(document.activeElement?.getAttribute('data-login-id')).toBe('work');
    await userEvent.keyboard('{ArrowRight}');
    await screen.findByRole('menu', { name: 'Codex Work configurations' });
    await userEvent.keyboard('{ArrowLeft}');
    await waitFor(() => expect(screen.queryByRole('menu', { name: 'Codex Work configurations' })).toBeNull());
    expect(document.activeElement?.getAttribute('data-login-id')).toBe('work');
    await userEvent.keyboard('{ArrowLeft}');
    await waitFor(() => expect(screen.queryByRole('menu', { name: 'Codex logins' })).toBeNull());
    expect(menu.isConnected).toBe(false);
    expect(document.activeElement).toBe(harnessRow);
  });

  it('lets the keyboard close a configuration submenu opened by hovering a login', async () => {
    render(<GroupedProviderMenu providers={[codex]} onSelect={vi.fn()} loginsEnabled />);
    const menu = await openLogins();
    await userEvent.hover(within(menu).getByRole('menuitem', { name: /^Default/ }));
    await screen.findByRole('menu', { name: 'Codex Default configurations' });
    expect(document.activeElement?.getAttribute('data-login-id')).toBe('default');
    await userEvent.keyboard('{Escape}');
    await waitFor(() => expect(screen.queryByRole('menu', { name: 'Codex Default configurations' })).toBeNull());
    expect(screen.getByRole('menu', { name: 'Codex logins' })).toBeTruthy();
    await userEvent.keyboard('{ArrowDown}');
    expect(document.activeElement?.getAttribute('data-login-id')).toBe('work');
  });

  it('leaves the menu unchanged with a single login', async () => {
    vi.mocked(api.listHarnessLogins).mockResolvedValue([DEFAULT]);
    render(<GroupedProviderMenu providers={[codex]} onSelect={vi.fn()} loginsEnabled />);
    await waitFor(() => expect(api.listHarnessLogins).toHaveBeenCalled());
    await userEvent.click(await screen.findByRole('button', { name: 'Codex configurations' }));
    expect(await screen.findByRole('menu', { name: 'Codex configurations' })).toBeTruthy();
    expect(screen.queryByRole('menu', { name: 'Codex logins' })).toBeNull();
  });

  it('never offers a login on a surface that cannot pass it on', async () => {
    render(<GroupedProviderMenu providers={[codex]} onSelect={vi.fn()} />);
    await userEvent.click(await screen.findByRole('button', { name: 'Codex configurations' }));
    expect(await screen.findByRole('menu', { name: 'Codex configurations' })).toBeTruthy();
    expect(api.listHarnessLogins).not.toHaveBeenCalled();
  });
});

describe('login submenu helpers', () => {
  it('offers a configuration under a login only when unpinned or pinned to it', () => {
    expect(offeredUnderLogin(configuration('a', 'A'), 'work')).toBe(true);
    expect(offeredUnderLogin(configuration('a', 'A', { login_id: 'work' }), 'work')).toBe(true);
    expect(offeredUnderLogin(configuration('a', 'A', { login_id: 'work' }), 'default')).toBe(false);
    expect(offeredUnderLogin(configuration('a', 'A', { login_id: 'work' }), undefined)).toBe(true);
    const routed = configuration('r', 'R', { spawn_option_id: 'codex:minimax', provider_route_id: 'minimax' });
    expect(offeredUnderLogin(routed, 'work')).toBe(false);
    expect(offeredUnderLogin(routed, 'default')).toBe(true);
  });

  it('places a flyout to the right of its row and flips left at the screen edge', () => {
    const row = { left: 100, right: 300, top: 50, bottom: 70 };
    expect(placeFlyout(row, 240, 100, 1000, 800)).toEqual({ left: 300, top: 50, maxHeight: 742 });
    expect(placeFlyout({ ...row, left: 700, right: 900 }, 240, 100, 1000, 800).left).toBe(460);
    // No usable room below and more above: hang above the row instead.
    expect(placeFlyout({ ...row, top: 760, bottom: 780 }, 240, 100, 1000, 800)).toEqual({ left: 300, top: 680, maxHeight: 772 });
  });
});
