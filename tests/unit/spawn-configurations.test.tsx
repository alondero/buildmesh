import { cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { GroupedProviderMenu } from '../../src/components/Providers/GroupedProviderMenu';
import type { SpawnOption } from '../../src/lib/groups';
import type { SpawnConfiguration } from '../../src/types/generated/SpawnConfiguration';
import * as api from '../../src/lib/tauri/provider';

vi.mock('../../src/lib/tauri/provider', () => ({
  listSpawnConfigurations: vi.fn(), getLaunchTargets: vi.fn(), saveSpawnConfiguration: vi.fn(), deleteSpawnConfiguration: vi.fn(),
}));

const target = { id: 'codex', harness_id: 'codex', harness_name: 'Codex', provider_name: 'OpenAI',
  models: [], efforts: ['low', 'high', 'max'], manual_model: true, supports_model: true, supports_extra_args: true };

const option: SpawnOption = {
  id: 'codex', label: 'Codex', harness_id: 'codex', provider_id: null,
  is_proxied: false, group_key: 'codex', color: '', icon: '',
  capabilities: {
    harness_id: 'codex', supports_resume: true, auto_resume_on_startup: true,
    requires_attention_hook: false, produces_readable_transcript: true,
    supports_passive_turn_watcher: false,
    attention_capability: { kind: 'none' },
    supports_model_override: true, supports_effort_override: true,
    supports_extra_args: true, supports_prefill: true, is_plain_terminal: false,
    effort_control: { kind: 'inline_config', key: 'effort', allowed: ['low', 'high', 'max'] },
    available_on: ['windows'],
  },
};
const saved: SpawnConfiguration = {
  id: 'sol', name: 'Sol Max', spawn_option_id: 'codex', model: 'gpt-5.6-sol', effort: 'max', extra_args: null,
};
afterEach(cleanup);
beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(api.listSpawnConfigurations).mockResolvedValue([saved]);
  vi.mocked(api.getLaunchTargets).mockResolvedValue([target]);
});

describe('Spawn configurations', () => {
  it('keeps default launch and selects a saved configuration with Alt preserved', async () => {
    const select = vi.fn();
    render(<GroupedProviderMenu providers={[option]} onSelect={select} />);
    const parent = screen.getByRole('menuitem', { name: 'Codex', exact: true });
    fireEvent.click(parent);
    expect(select).toHaveBeenLastCalledWith('codex', false);
    fireEvent.mouseEnter(parent);
    fireEvent.click(await screen.findByRole('menuitem', { name: 'Sol Max' }), { altKey: true });
    expect(select).toHaveBeenLastCalledWith('codex', true, 'sol');
  });

  it('creates a model-only configuration and exposes only declared effort values', async () => {
    vi.mocked(api.listSpawnConfigurations).mockResolvedValue([]);
    vi.mocked(api.saveSpawnConfiguration).mockResolvedValue({ ...saved, name: 'Sol', effort: null });
    render(<GroupedProviderMenu providers={[option]} onSelect={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Codex configurations', exact: true }));
    await waitFor(() => expect((screen.getByRole('menuitem', { name: /New configuration/ }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole('menuitem', { name: /New configuration/ }));
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'Sol' } });
    fireEvent.keyDown(screen.getByLabelText('Name'), { key: 'Tab' });
    fireEvent.change(await screen.findByLabelText('Model'), { target: { value: 'gpt-5.6-sol' } });
    expect(within(screen.getByLabelText('Effort')).getAllByRole('option').map((o) => o.textContent)).toEqual(['Default', 'low', 'high', 'max']);
    const cancel = screen.getByRole('button', { name: 'Cancel' });
    fireEvent.focus(cancel);
    fireEvent.keyDown(cancel, { key: 'Tab' });
    expect(document.activeElement).toBe(screen.getByLabelText('Name'));
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(api.saveSpawnConfiguration).toHaveBeenCalled());
    expect(api.saveSpawnConfiguration).toHaveBeenCalledWith({
      id: '', name: 'Sol', spawn_option_id: 'codex', model: 'gpt-5.6-sol', effort: null, extra_args: null,
    });
  });

  it('dismisses the browsing flyout on Tab and keeps parent Escape local to the flyout', async () => {
    const onClose = vi.fn();
    render(<GroupedProviderMenu providers={[option]} onSelect={vi.fn()} onClose={onClose} />);
    const parent = screen.getByRole('menuitem', { name: 'Codex', exact: true });
    fireEvent.mouseEnter(parent);
    const panel = await screen.findByTestId('spawn-configurations');
    const first = within(panel).getByRole('menuitem', { name: 'Spawn with defaults' });
    fireEvent.keyDown(first, { key: 'Tab' });
    expect(screen.queryByTestId('spawn-configurations')).toBeNull();

    onClose.mockClear();
    fireEvent.mouseEnter(parent);
    await screen.findByTestId('spawn-configurations');
    fireEvent.keyDown(parent, { key: 'Escape' });
    expect(onClose).not.toHaveBeenCalled();
    expect(screen.queryByTestId('spawn-configurations')).toBeNull();
  });

  it('dismisses the browsing flyout on Tab inside a dialog', async () => {
    render(<div role="dialog"><GroupedProviderMenu providers={[option]} onSelect={vi.fn()} /></div>);
    fireEvent.click(screen.getByRole('button', { name: 'Codex configurations', exact: true }));
    const first = within(await screen.findByTestId('spawn-configurations')).getByRole('menuitem', { name: 'Spawn with defaults' });
    fireEvent.keyDown(first, { key: 'Tab' });
    expect(screen.queryByTestId('spawn-configurations')).toBeNull();
  });

  it('uses one roving tab stop for configuration menu items and moves it with arrows', async () => {
    render(<GroupedProviderMenu providers={[option]} onSelect={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Codex configurations', exact: true }));
    const panel = await screen.findByTestId('spawn-configurations');
    const items = within(panel).getAllByRole('menuitem');
    expect(items.filter((item) => item.tabIndex === 0)).toHaveLength(1);
    expect(items.filter((item) => item.tabIndex === -1)).toHaveLength(items.length - 1);
    fireEvent.keyDown(items[0], { key: 'ArrowDown' });
    expect(document.activeElement).toBe(items[1]);
    expect(items[1].tabIndex).toBe(0);
  });

  it('retains failed edits for retry and reports failed deletes', async () => {
    vi.mocked(api.saveSpawnConfiguration).mockRejectedValue(new Error('disk full'));
    render(<GroupedProviderMenu providers={[option]} onSelect={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Codex configurations', exact: true }));
    fireEvent.click(await screen.findByRole('menuitem', { name: 'Edit Sol Max' }));
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'Renamed' } });
    await waitFor(() => expect((screen.getByRole('button', { name: 'Save' }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    expect((await screen.findByRole('alert')).textContent).toContain('disk full');
    expect((screen.getByLabelText('Name') as HTMLInputElement).value).toBe('Renamed');
    vi.mocked(api.deleteSpawnConfiguration).mockRejectedValueOnce(new Error('delete failed')).mockResolvedValueOnce();
    fireEvent.click(screen.getByRole('button', { name: 'Delete' }));
    fireEvent.click(within(screen.getByRole('dialog', { name: 'Delete Launch Configuration?' })).getByRole('button', { name: 'Delete' }));
    await waitFor(() => expect(screen.getByRole('alert').textContent).toContain('delete failed'));
    fireEvent.click(screen.getByRole('button', { name: 'Delete' }));
    fireEvent.click(within(screen.getByRole('dialog', { name: 'Delete Launch Configuration?' })).getByRole('button', { name: 'Delete' }));
    await waitFor(() => expect(screen.queryByTestId('spawn-configurations')).toBeNull());
    expect(api.deleteSpawnConfiguration).toHaveBeenCalledWith('sol');
  });

  it('does not expose effort or extra arguments when unsupported', async () => {
    const limited = { ...option, capabilities: { ...option.capabilities!, supports_effort_override: false, supports_extra_args: false, effort_control: { kind: 'none' as const } } };
    vi.mocked(api.getLaunchTargets).mockResolvedValue([{ ...target, efforts: [], supports_extra_args: false }]);
    render(<GroupedProviderMenu providers={[limited]} onSelect={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Codex configurations', exact: true }));
    await waitFor(() => expect((screen.getByRole('menuitem', { name: /New configuration/ }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole('menuitem', { name: /New configuration/ }));
    expect(await screen.findByLabelText('Model')).toBeTruthy();
    expect(screen.queryByLabelText('Effort')).toBeNull();
    expect(screen.queryByLabelText('Extra arguments')).toBeNull();
  });

  it('ignores an old response after switching harnesses and returns keyboard focus to the parent', async () => {
    let resolveOld!: (value: SpawnConfiguration[]) => void;
    vi.mocked(api.listSpawnConfigurations).mockReturnValueOnce(new Promise((r) => { resolveOld = r; })).mockResolvedValueOnce([]);
    const second = { ...option, id: 'claude', harness_id: 'claude', group_key: 'claude', label: 'Claude Code' };
    render(<GroupedProviderMenu providers={[option, second]} onSelect={vi.fn()} />);
    fireEvent.keyDown(screen.getByRole('menuitem', { name: 'Codex', exact: true }), { key: 'ArrowRight' });
    const parent = screen.getByRole('menuitem', { name: 'Claude Code' });
    fireEvent.mouseEnter(parent);
    await screen.findByText('No saved configurations');
    resolveOld([saved]);
    expect(screen.queryByText('Sol Max')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Claude Code configurations', exact: true }));
    fireEvent.keyDown(screen.getByRole('menuitem', { name: 'Spawn with defaults' }), { key: 'ArrowLeft' });
    expect(document.activeElement).toBe(parent);
    expect(screen.queryByTestId('spawn-configurations')).toBeNull();
  });
});
