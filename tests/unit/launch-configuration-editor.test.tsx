import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { LaunchConfigurationEditor } from '../../src/components/Providers/LaunchConfigurationEditor';
import type { LaunchTarget } from '../../src/types/generated/LaunchTarget';
import { LaunchConfigurations } from '../../src/components/Providers/LaunchConfigurations';
import type { ProviderPairing } from '../../src/types/generated/ProviderPairing';
import type { SpawnConfiguration } from '../../src/types/generated/SpawnConfiguration';

const targets: LaunchTarget[] = [{ id: 'claude:minimax', harness_id: 'claude', harness_name: 'Claude Code', provider_name: 'MiniMax', models: [{ id: 'MiniMax-M3', name: 'MiniMax M3', surface: 'anthropic', efforts: [] }], efforts: ['low', 'high'], route_attached: false, manual_model: false, supports_model: true, supports_extra_args: true }];
const route: ProviderPairing = { harness_id: 'claude', provider_id: 'minimax', surface: 'anthropic', base_url: 'https://api.minimax.io/anthropic', model_tiers: { default: 'MiniMax-M3', opus: null, fable: null, sonnet: null, haiku: null, small_fast: null } };

describe('Launch Configuration editor', () => {
  it('shows effort guidance for WSL profiles and scopes max guidance to the max selection', () => {
    const claude: LaunchTarget = {
      id: 'claude-wsl-ubuntu', harness_id: 'claude-wsl-ubuntu', harness_name: 'Claude Code (WSL: Ubuntu)', provider_name: 'Native authentication',
      models: [], efforts: ['low', 'medium', 'high', 'xhigh', 'max'], route_attached: false,
      manual_model: true, supports_model: true, supports_extra_args: true,
    };
    render(<LaunchConfigurationEditor
      value={{ id: '', name: '', spawn_option_id: 'claude-wsl-ubuntu', model: null, effort: null, extra_args: null }}
      targets={[claude]} onSave={vi.fn()} onCancel={vi.fn()}
    />);
    const effort = screen.getByLabelText('Effort');
    expect(within(effort).getAllByRole('option').map((option) => option.textContent))
      .toEqual(['Default', 'low', 'medium', 'high', 'xhigh', 'max']);
    expect(effort.getAttribute('aria-describedby')).toBe('launch-config-effort-help');
    expect(screen.getByText('Availability depends on the selected model.')).toBeTruthy();
    expect(screen.queryByText('Availability depends on the selected model. Max applies to the launched session.')).toBeNull();

    fireEvent.change(effort, { target: { value: 'max' } });
    expect(screen.getByText('Availability depends on the selected model. Max applies to the launched session.')).toBeTruthy();

    fireEvent.change(effort, { target: { value: 'high' } });
    expect(screen.getByText('Availability depends on the selected model.')).toBeTruthy();
    expect(screen.queryByText('Availability depends on the selected model. Max applies to the launched session.')).toBeNull();
  });

  it('saves an unattached pairing together with the configuration', async () => {
    const save = vi.fn().mockResolvedValue(undefined);
    render(<LaunchConfigurationEditor value={{ id: '', name: 'My proxy', spawn_option_id: route.harness_id + ':' + route.provider_id, model: null, effort: null, extra_args: null }} targets={[{ ...targets[0], route, route_attached: false }]} onSave={save} onCancel={vi.fn()} />);
    expect((screen.getByLabelText('Provider endpoint') as HTMLInputElement).value).toBe(route.base_url);
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(save).toHaveBeenCalledWith(expect.objectContaining({ name: 'My proxy' }), route, expect.any(Function)));
  });

  it('cancels a custom pairing without saving and resets endpoint when switching providers', () => {
    const save = vi.fn();
    const cancel = vi.fn();
    const native = { ...targets[0], id: 'claude', provider_name: 'Native authentication', route: undefined };
    render(<LaunchConfigurationEditor value={{ id: '', name: 'Proxy', spawn_option_id: targets[0].id, model: null, effort: null, extra_args: null }} targets={[native, { ...targets[0], route, route_attached: false }]} onSave={save} onCancel={cancel} />);
    fireEvent.change(screen.getByLabelText('Provider endpoint'), { target: { value: 'https://other.example' } });
    fireEvent.change(screen.getByLabelText('Provider'), { target: { value: 'claude' } });
    expect(screen.queryByLabelText('Provider endpoint')).toBeNull();
    fireEvent.change(screen.getByLabelText('Provider'), { target: { value: targets[0].id } });
    expect((screen.getByLabelText('Provider endpoint') as HTMLInputElement).value).toBe(route.base_url);
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(cancel).toHaveBeenCalledOnce();
    expect(save).not.toHaveBeenCalled();
  });

  it('verifies the selected configuration and invalidates displayed success on model change', async () => {
    const codexRoute = { ...route, harness_id: 'codex', surface: 'openai' as const, base_url: 'https://api.minimax.io/v1' };
    const codex = { ...targets[0], id: 'codex:minimax', harness_id: 'codex', route: codexRoute, route_attached: true, verification_required: true };
    const verify = vi.fn().mockResolvedValue({ status: 'verified', model_id: 'MiniMax-M3', runtime: 'native-windows' });
    let finishSave!: () => void;
    let startRefresh!: () => void;
    const save = vi.fn((_value: SpawnConfiguration, _route: ProviderPairing | undefined, onRefreshing: (() => void) | undefined) => new Promise<void>((resolve) => {
      finishSave = resolve;
      startRefresh = onRefreshing!;
    }));
    render(<LaunchConfigurationEditor value={{ id: '', name: 'Proxy', spawn_option_id: codex.id, model: 'MiniMax-M3', effort: null, extra_args: null }} targets={[codex]} onVerify={verify} onSave={save} onCancel={vi.fn()} />);
    const status = screen.getByRole('status');
    fireEvent.click(screen.getByRole('button', { name: 'Verify provider and model' }));
    await screen.findByText('Verified MiniMax-M3 for native-windows');
    expect(verify).toHaveBeenCalledWith(expect.objectContaining({ model: 'MiniMax-M3', spawn_option_id: 'codex:minimax' }), undefined);
    expect(screen.getAllByRole('status')).toHaveLength(1);
    expect(status.textContent).toContain('Verified MiniMax-M3');
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(screen.getAllByRole('status')).toHaveLength(1);
    expect(status.textContent).toBe('Saving configuration…');
    await act(async () => { startRefresh(); });
    expect(status.textContent).toBe('Refreshing availability…');
    await act(async () => { finishSave(); });
    expect(screen.getAllByRole('status')).toHaveLength(1);
    expect(status.textContent).toContain('Verified MiniMax-M3');
    fireEvent.change(screen.getByLabelText('Model'), { target: { value: '__custom__' } });
    fireEvent.change(screen.getByLabelText('Custom model'), { target: { value: 'new-model' } });
    expect(screen.getByRole('status').textContent).toContain('verify again');
  });
  it('shows effort choices for the provider default model', () => {
    const codex = { ...targets[0], id: 'codex:minimax', harness_id: 'codex', harness_name: 'Codex',
      default_model: 'MiniMax-M3', efforts: [],
      models: [{ id: 'MiniMax-M3', name: 'M3', surface: 'openai' as const, efforts: ['none', 'high'] }] };
    render(<LaunchConfigurationEditor value={{ id: '', name: 'M3', spawn_option_id: codex.id, model: null, effort: null, extra_args: null }} targets={[codex]} onSave={vi.fn()} onCancel={vi.fn()} />);
    expect(screen.getByLabelText('Effort')).toBeTruthy();
    expect(within(screen.getByLabelText('Effort')).getAllByRole('option').map(o => o.textContent)).toEqual(['Default', 'none', 'high']);
  });
  it('only deletes after confirmation and preserves the draft on cancellation', async () => {
    const remove = vi.fn().mockResolvedValue(undefined);
    render(<LaunchConfigurationEditor value={{ id: 'launch/test', name: 'Keep me', spawn_option_id: 'claude:minimax', model: null, effort: null, extra_args: null }} targets={targets} onSave={vi.fn()} onDelete={remove} onCancel={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Delete' }));
    expect(remove).not.toHaveBeenCalled();
    fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Cancel' }));
    expect((screen.getByLabelText('Name') as HTMLInputElement).value).toBe('Keep me');
    fireEvent.click(screen.getByRole('button', { name: 'Delete' }));
    fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Delete' }));
    await waitFor(() => expect(remove).toHaveBeenCalledOnce());
  });
  it('preserves a draft after a failed save and allows retry', async () => {
    let rejectSave!: (error: Error) => void;
    const save = vi.fn().mockReturnValueOnce(new Promise<void>((_resolve, reject) => { rejectSave = reject; })).mockResolvedValue(undefined);
    render(<LaunchConfigurationEditor value={{ id: 'launch/test', name: 'My recipe', spawn_option_id: 'claude:minimax', model: 'MiniMax-M3', effort: null, extra_args: null }} targets={targets} onSave={save} onCancel={() => {}} />);
    expect(screen.getByRole('status').textContent).toBe('');
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect(screen.getByRole('status').textContent).toBe('Saving configuration…');
    await act(async () => { rejectSave(new Error('Route unavailable')); });
    expect(screen.getByRole('status').textContent).toBe('');
    expect(screen.getByRole('button', { name: 'Save' }).closest('fieldset')?.disabled).toBe(false);
    expect(screen.getByRole('alert').textContent).toContain('Route unavailable');
    expect((screen.getByLabelText('Name') as HTMLInputElement).value).toBe('My recipe');
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(save).toHaveBeenCalledTimes(2));
  });

  it('clones a generated configuration without its ownership or snapshot', async () => {
    const original = { id: 'launch/generated', name: 'MiniMax', spawn_option_id: 'claude:minimax', model: 'MiniMax-M3', effort: null, extra_args: null, generated: { source: 'claude:minimax', catalogue_revision: 1, user_owned: false } };
    const api = { list: vi.fn().mockResolvedValue([original]), targets: vi.fn().mockResolvedValue(targets), save: vi.fn().mockResolvedValue({ ...original, id: 'launch/copy' }), remove: vi.fn() };
    render(<LaunchConfigurations api={api} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Clone MiniMax' }));
    expect((screen.getByLabelText('Name') as HTMLInputElement).value).toBe('MiniMax copy');
    expect(screen.getByText('User-owned configuration')).not.toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(api.save).toHaveBeenCalledWith(expect.objectContaining({ id: '', generated: undefined, resolved: undefined })));
    await screen.findByRole('button', { name: 'Clone MiniMax' });
  });

  it('ignores a stale list response after the data source changes', async () => {
    let settle!: (value: never[]) => void;
    const oldApi = { list: () => new Promise<never[]>(resolve => { settle = resolve; }), targets: async () => targets, save: vi.fn(), remove: vi.fn() };
    const newApi = { ...oldApi, list: async () => [{ id: 'new', name: 'Latest', spawn_option_id: 'claude:minimax', model: null, effort: null, extra_args: null }] };
    const { rerender } = render(<LaunchConfigurations api={oldApi} />);
    await waitFor(() => expect(settle).toBeTypeOf('function'));
    rerender(<LaunchConfigurations api={newApi} />);
    await screen.findByRole('button', { name: 'Edit Latest' });
    settle([]);
    await waitFor(() => expect(screen.getByRole('button', { name: 'Edit Latest' })).not.toBeNull());
  });
  it('saves the catalogue model identity and hides unsupported effort', async () => {
    const save = vi.fn().mockResolvedValue(undefined);
    render(<LaunchConfigurationEditor value={{ id: '', name: '', spawn_option_id: 'claude:minimax', model: null, effort: null, extra_args: null }} targets={targets} onSave={save} onCancel={() => {}} />);
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'Fast review' } });
    fireEvent.change(screen.getByLabelText('Model'), { target: { value: 'MiniMax-M3' } });
    expect(screen.queryByLabelText('Effort')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(save).toHaveBeenCalledWith(expect.objectContaining({ name: 'Fast review', model: 'MiniMax-M3', effort: null }), undefined, expect.any(Function)));
  });
});
