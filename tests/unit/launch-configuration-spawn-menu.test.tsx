import { afterEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, render, screen, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { GroupedProviderMenu } from '../../src/components/Providers/GroupedProviderMenu';
import type { SpawnOption } from '../../src/lib/groups';
import * as api from '../../src/lib/tauri/provider';
import type { SpawnConfiguration } from '../../src/types/generated/SpawnConfiguration';
import type { ProviderInfo } from '../../src/types/generated/ProviderInfo';
import { backgroundInferenceOption } from '../../src/lib/backgroundInference';
import { HARNESS_CAPABILITIES } from '../../src/types/generated/HarnessCapabilitiesTable';

vi.mock('../../src/lib/tauri/provider', () => ({
  listSpawnConfigurations: vi.fn().mockResolvedValue([
    { id: 'launch/claude:minimax', name: 'MiniMax', spawn_option_id: 'claude:minimax', model: null, effort: null, extra_args: null },
    { id: 'launch/codex-sol', name: 'Codex Sol', spawn_option_id: 'codex', model: 'gpt-5.6-sol', effort: null, extra_args: null },
  ]),
  verifyLaunchConfiguration: vi.fn(), getLaunchTargets: vi.fn().mockResolvedValue([]),
  saveSpawnConfiguration: vi.fn(),
  deleteSpawnConfiguration: vi.fn(),
  listProviders: vi.fn(),
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
  it('keeps background restrictions on submenu defaults and loaded configurations', async () => {
    const onSelect = vi.fn();
    const unsupported = { ...row('freebuff', 'freebuff'), capabilities: HARNESS_CAPABILITIES.freebuff };
    vi.mocked(api.listSpawnConfigurations).mockResolvedValueOnce([
      configuration('launch/freebuff', 'Freebuff recipe', 'freebuff'),
    ]);
    render(<GroupedProviderMenu providers={[backgroundInferenceOption(unsupported)]}
      decorate={backgroundInferenceOption} onSelect={onSelect} />);
    await userEvent.click(screen.getByRole('button', { name: 'freebuff configurations' }));
    const defaults = screen.getByRole('menuitem', { name: 'Spawn with defaults' });
    expect(defaults.getAttribute('aria-disabled')).toBe('true');
    await userEvent.click(defaults);
    const saved = await screen.findByRole('menuitem', { name: /^Freebuff recipe/ });
    expect(saved.getAttribute('aria-disabled')).toBe('true');
    await userEvent.click(saved);
    expect(onSelect).not.toHaveBeenCalled();
  });

  it('retains background restrictions after refreshing an edited configuration', async () => {
    const saved: SpawnConfiguration = { ...configuration('launch/new', 'New Codex', 'codex'), extra_args: '--json' };
    const native = { ...row('codex', 'codex'), capabilities: HARNESS_CAPABILITIES.codex };
    vi.mocked(api.getLaunchTargets).mockResolvedValueOnce([
      { id: 'codex', harness_id: 'codex', harness_name: 'Codex', provider_name: 'OpenAI', models: [], efforts: [], route_attached: false, manual_model: true, supports_model: true, supports_extra_args: true },
    ]);
    vi.mocked(api.saveSpawnConfiguration).mockResolvedValueOnce(saved);
    vi.mocked(api.listProviders).mockResolvedValueOnce([{
      ...native, id: saved.id, color: '#fff', resumable: true, configuration: saved,
    }]);
    const onSelect = vi.fn();
    render(<GroupedProviderMenu providers={[native]} decorate={backgroundInferenceOption} onSelect={onSelect} />);
    await userEvent.click(screen.getByRole('button', { name: 'codex configurations' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: 'New configuration…' }));
    await userEvent.type(screen.getByLabelText('Name'), 'New Codex');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));
    const choice = await screen.findByRole('menuitem', { name: /^New Codex/ });
    expect(choice.getAttribute('aria-disabled')).toBe('true');
    expect(choice.textContent).toContain('remove extra CLI arguments');
    await userEvent.click(choice);
    expect(onSelect).not.toHaveBeenCalled();
  });

  it('disables routed background configurations loaded without a provider-menu snapshot', async () => {
    const native = { ...row('codex', 'codex'), capabilities: HARNESS_CAPABILITIES.codex };
    vi.mocked(api.listSpawnConfigurations).mockResolvedValueOnce([
      { ...configuration('launch/routed', 'Routed Codex', 'codex:custom'), provider_route_id: 'custom-route' },
    ]);
    const onSelect = vi.fn();
    render(<GroupedProviderMenu providers={[native]} decorate={backgroundInferenceOption} onSelect={onSelect} />);
    await userEvent.click(screen.getByRole('button', { name: 'codex configurations' }));
    const choice = await screen.findByRole('menuitem', { name: /^Routed Codex/ });
    expect(choice.getAttribute('aria-disabled')).toBe('true');
    expect(choice.textContent).toContain('native authentication');
    await userEvent.click(choice);
    expect(onSelect).not.toHaveBeenCalled();
  });
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
      { id: 'codex', harness_id: 'codex', harness_name: 'Codex', provider_name: 'OpenAI', models: [], efforts: [], route_attached: false, manual_model: true, supports_model: true, supports_extra_args: true },
      { id: 'codex:minimax', harness_id: 'codex', harness_name: 'Codex', provider_name: 'MiniMax', models: [], efforts: [], route_attached: false, manual_model: true, supports_model: true, supports_extra_args: false },
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

  it('shows saving progress until the spawn menu has refreshed the saved configuration', async () => {
    const saved: SpawnConfiguration = { id: 'launch/new', name: 'New Codex', spawn_option_id: 'codex', model: null, effort: null, extra_args: null };
    const refreshedProvider = { id: saved.id, harness_id: 'codex', unavailable_reason: undefined } as unknown as ProviderInfo;
    let finishSave!: (value: SpawnConfiguration) => void;
    let finishRefresh!: () => void;
    vi.mocked(api.getLaunchTargets).mockResolvedValueOnce([
      { id: 'codex', harness_id: 'codex', harness_name: 'Codex', provider_name: 'OpenAI', models: [], efforts: [], route_attached: false, manual_model: true, supports_model: true, supports_extra_args: true },
    ]);
    vi.mocked(api.saveSpawnConfiguration).mockReturnValueOnce(new Promise((resolve) => { finishSave = resolve; }));
    vi.mocked(api.listProviders).mockReturnValueOnce(new Promise((resolve) => { finishRefresh = () => resolve([refreshedProvider]); }));
    render(<GroupedProviderMenu providers={[
      row('codex', 'codex'),
      { ...row('launch/existing', 'codex'), configuration: configuration('launch/existing', 'Existing', 'codex') },
    ]} onSelect={vi.fn()} />);

    await userEvent.click(screen.getByRole('button', { name: 'codex configurations' }));
    await userEvent.click(await screen.findByRole('menuitem', { name: 'New configuration…' }));
    await userEvent.type(screen.getByLabelText('Name'), 'New Codex');
    const status = screen.getByRole('status');
    expect(status.textContent).toBe('');
    await userEvent.click(screen.getByRole('button', { name: 'Save' }));

    expect(status.textContent).toBe('Saving configuration…');
    expect(screen.getByRole('button', { name: 'Saving…' }).closest('fieldset')?.disabled).toBe(true);
    await act(async () => { finishSave(saved); });
    expect(status.textContent).toBe('Refreshing availability…');
    await act(async () => { finishRefresh(); });
    expect(screen.queryByRole('form', { name: 'Launch Configuration' })).toBeNull();
    expect(status.isConnected).toBe(false);
    expect(await screen.findByRole('menuitem', { name: 'Edit New Codex' })).toBeTruthy();
  });

  it('emits separate save and refresh timing checkpoints without configuration values', async () => {
    const saved: SpawnConfiguration = { id: 'launch/new', name: 'New Codex', spawn_option_id: 'codex', model: null, effort: null, extra_args: null };
    const refreshedProvider = { id: saved.id, harness_id: 'codex', unavailable_reason: undefined } as unknown as ProviderInfo;
    vi.mocked(api.getLaunchTargets).mockResolvedValueOnce([
      { id: 'codex', harness_id: 'codex', harness_name: 'Codex', provider_name: 'OpenAI', models: [], efforts: [], route_attached: false, manual_model: true, supports_model: true, supports_extra_args: true },
    ]);
    vi.mocked(api.saveSpawnConfiguration).mockResolvedValueOnce(saved);
    vi.mocked(api.listProviders).mockResolvedValueOnce([refreshedProvider]);
    const info = vi.spyOn(console, 'info').mockImplementation(() => {});
    try {
      render(<GroupedProviderMenu providers={[
        row('codex', 'codex'),
        { ...row('launch/existing', 'codex'), configuration: configuration('launch/existing', 'Existing', 'codex') },
      ]} onSelect={vi.fn()} />);

      await userEvent.click(screen.getByRole('button', { name: 'codex configurations' }));
      await userEvent.click(await screen.findByRole('menuitem', { name: 'New configuration…' }));
      await userEvent.type(screen.getByLabelText('Name'), 'New Codex');
      await userEvent.click(screen.getByRole('button', { name: 'Save' }));
      expect(await screen.findByRole('menuitem', { name: 'Edit New Codex' })).toBeTruthy();

      const lines = info.mock.calls.map((args) => String(args[0])).filter((line) => line.startsWith('launch_config_timing:'));
      expect(lines).toHaveLength(2);
      expect(lines[0]).toMatch(/^launch_config_timing: checkpoint=save elapsed=\d+ms$/);
      expect(lines[1]).toMatch(/^launch_config_timing: checkpoint=refresh elapsed=\d+ms$/);
      for (const line of lines) {
        expect(line).not.toContain('New Codex');
        expect(line).not.toContain('launch/new');
      }
    } finally {
      info.mockRestore();
    }
  });
});
