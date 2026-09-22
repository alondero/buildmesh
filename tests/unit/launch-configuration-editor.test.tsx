import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';
import { LaunchConfigurationEditor } from '../../src/components/Providers/LaunchConfigurationEditor';
import type { LaunchTarget } from '../../src/types/generated/LaunchTarget';
import { LaunchConfigurations } from '../../src/components/Providers/LaunchConfigurations';

const targets: LaunchTarget[] = [{ id: 'claude:minimax', harness_id: 'claude', harness_name: 'Claude Code', provider_name: 'MiniMax', models: [{ id: 'MiniMax-M3', name: 'MiniMax M3', surface: 'anthropic', efforts: [] }], efforts: ['low', 'high'], manual_model: false, supports_model: true, supports_extra_args: true }];

describe('Launch Configuration editor', () => {
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
    const save = vi.fn().mockRejectedValueOnce(new Error('Route unavailable')).mockResolvedValue(undefined);
    render(<LaunchConfigurationEditor value={{ id: 'launch/test', name: 'My recipe', spawn_option_id: 'claude:minimax', model: 'MiniMax-M3', effort: null, extra_args: null }} targets={targets} onSave={save} onCancel={() => {}} />);
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect((await screen.findByRole('alert')).textContent).toContain('Route unavailable');
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
    await waitFor(() => expect(save).toHaveBeenCalledWith(expect.objectContaining({ name: 'Fast review', model: 'MiniMax-M3', effort: null })));
  });
});
