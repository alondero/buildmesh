import { afterEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { GroupedProviderMenu } from '../../src/components/Providers/GroupedProviderMenu';
import type { SpawnOption } from '../../src/lib/groups';
import * as api from '../../src/lib/tauri/provider';

vi.mock('../../src/lib/tauri/provider', () => ({
  listSpawnConfigurations: vi.fn().mockResolvedValue([
    { id: 'launch/claude:minimax', name: 'MiniMax', spawn_option_id: 'claude:minimax', model: null, effort: null, extra_args: null },
    { id: 'launch/codex-sol', name: 'Codex Sol', spawn_option_id: 'codex', model: 'gpt-5.6-sol', effort: null, extra_args: null },
  ]),
  getLaunchTargets: vi.fn().mockResolvedValue([]),
  saveSpawnConfiguration: vi.fn(),
  deleteSpawnConfiguration: vi.fn(),
}));

const row = (id: string, harness: string, provider: string | null = null): SpawnOption => ({
  id, label: id, icon: 'X', color: 'bg-gray-500', harness_id: harness,
  provider_id: provider, is_proxied: provider !== null, group_key: harness,
});
const configuration = (id: string, name: string, option: string) => ({
  id, name, spawn_option_id: option, model: null, effort: null, extra_args: null,
});

afterEach(() => cleanup());

describe('launch configurations in the spawn menu', () => {
  it('never restores flat provider routes after the last recipe is deleted', () => {
    render(<GroupedProviderMenu providers={[row('claude', 'claude'), row('claude:minimax', 'claude', 'minimax')]} onSelect={vi.fn()} />);
    const root = screen.getByRole('menu', { name: 'Select a provider' });
    expect(within(root).getAllByRole('menuitem').map((item) => item.getAttribute('data-spawn-id'))).toEqual(['claude']);
  });

  it('shows harnesses as parents and launches Codex and proxied recipes from their submenus', async () => {
    const onSelect = vi.fn();
    render(<GroupedProviderMenu providers={[
      row('claude', 'claude'),
      row('claude:minimax', 'claude', 'minimax'),
      { ...row('launch/claude:minimax', 'claude', 'minimax'), configuration: configuration('launch/claude:minimax', 'MiniMax', 'claude:minimax') },
      row('codex', 'codex'),
      { ...row('launch/codex-sol', 'codex'), configuration: configuration('launch/codex-sol', 'Codex Sol', 'codex') },
    ]} onSelect={onSelect} />);

    const root = screen.getByRole('menu', { name: 'Select a provider' });
    expect(within(root).getAllByRole('menuitem').map((item) => item.getAttribute('data-spawn-id'))).toEqual(['claude', 'codex']);
    await userEvent.click(screen.getByRole('button', { name: 'codex configurations' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: 'Codex Sol' }));
    expect(onSelect).toHaveBeenCalledWith('codex', false, 'launch/codex-sol');

    await userEvent.click(screen.getByRole('button', { name: 'claude configurations' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: 'MiniMax' }));
    expect(onSelect).toHaveBeenCalledWith('claude:minimax', false, 'launch/claude:minimax');
  });

  it('keeps an unavailable Codex recipe visible with its reason', async () => {
    const onSelect = vi.fn();
    render(<GroupedProviderMenu providers={[
      { ...row('codex', 'codex'), unavailable_reason: 'Harness is unavailable' },
      { ...row('launch/codex-sol', 'codex'), configuration: configuration('launch/codex-sol', 'Codex Sol', 'codex'), unavailable_reason: 'Harness is unavailable' },
    ]} onSelect={onSelect} />);

    await userEvent.click(screen.getByRole('button', { name: 'codex configurations' }));
    const recipe = await screen.findByRole('menuitem', { name: /^Codex Sol/ });
    expect(recipe.textContent).toContain('Harness is unavailable');
    expect(recipe.getAttribute('aria-disabled')).toBe('true');
    await userEvent.click(recipe);
    expect(onSelect).not.toHaveBeenCalled();
  });

  it('opens the catalogue-backed editor from a Codex recipe', async () => {
    vi.mocked(api.getLaunchTargets).mockResolvedValueOnce([
      { id: 'codex', harness_id: 'codex', harness_name: 'Codex', provider_name: 'OpenAI', models: [], efforts: [], manual_model: true, supports_model: true, supports_extra_args: true },
      { id: 'codex:minimax', harness_id: 'codex', harness_name: 'Codex', provider_name: 'MiniMax', models: [], efforts: [], manual_model: true, supports_model: true, supports_extra_args: false },
    ]);
    render(<GroupedProviderMenu providers={[
      row('codex', 'codex'),
      { ...row('launch/codex-sol', 'codex'), configuration: configuration('launch/codex-sol', 'Codex Sol', 'codex') },
    ]} onSelect={vi.fn()} />);
    await userEvent.click(screen.getByRole('button', { name: 'codex configurations' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: 'Edit Codex Sol' }));
    const editor = screen.getByRole('form', { name: 'Launch Configuration' });
    expect(within(editor).getByLabelText('Harness')).toBeTruthy();
    expect((within(editor).getByLabelText('Provider') as HTMLSelectElement).value).toBe('codex');
    expect(within(editor).getByRole('option', { name: 'MiniMax' })).toBeTruthy();
  });
});
