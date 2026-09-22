import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { GroupedProviderMenu } from '../../src/components/Providers/GroupedProviderMenu';
import type { SpawnOption } from '../../src/lib/groups';
import type { SpawnConfiguration } from '../../src/types/generated/SpawnConfiguration';
import * as api from '../../src/lib/tauri/provider';

vi.mock('../../src/lib/tauri/provider', () => ({
  listProviders: vi.fn(), listSpawnConfigurations: vi.fn(), getLaunchTargets: vi.fn(), saveSpawnConfiguration: vi.fn(), deleteSpawnConfiguration: vi.fn(),
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
const savedRow = { ...option, id: saved.id, label: saved.name, configuration: saved };
const claudeTarget = { ...target, id: 'claude', harness_id: 'claude', harness_name: 'Claude Code' };
afterEach(cleanup);
beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(api.listProviders).mockResolvedValue([option, savedRow]);
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
    expect(await screen.findByRole('menuitem', { name: 'Sol' })).toBeTruthy();
    expect(screen.getByTestId('spawn-configurations')).toBeTruthy();
    expect(api.saveSpawnConfiguration).toHaveBeenCalled();
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
    await screen.findByText('No saved configurations');
    expect(api.deleteSpawnConfiguration).toHaveBeenCalledWith('sol');
  });

  it('removes a saved recipe from its old submenu when its harness changes', async () => {
    const moved = { ...saved, spawn_option_id: 'claude', harness_id: 'claude' };
    const movedRow = { ...option, id: saved.id, label: saved.name, harness_id: 'claude', group_key: 'claude', configuration: moved };
    vi.mocked(api.getLaunchTargets).mockResolvedValue([target, claudeTarget]);
    vi.mocked(api.saveSpawnConfiguration).mockResolvedValue(moved);
    vi.mocked(api.listProviders).mockResolvedValue([option, movedRow]);
    render(<GroupedProviderMenu providers={[option]} onSelect={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Codex configurations', exact: true }));
    fireEvent.click(await screen.findByRole('menuitem', { name: 'Edit Sol Max' }));
    await screen.findByRole('option', { name: 'Claude Code' });
    fireEvent.change(screen.getByLabelText('Harness'), { target: { value: 'claude' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));

    await waitFor(() => expect(api.saveSpawnConfiguration).toHaveBeenCalled());
    expect(api.listProviders).toHaveBeenCalled();
    await screen.findByText('No saved configurations');
    expect(screen.queryByRole('menuitem', { name: 'Sol Max' })).toBeNull();
    expect(api.saveSpawnConfiguration).toHaveBeenCalledWith(expect.objectContaining({ spawn_option_id: 'claude' }));
  });

  it('refreshes recipe availability after an edit', async () => {
    const unavailableRow = { ...savedRow, unavailable_reason: 'Harness is unavailable' };
    const refreshedRow = { ...savedRow, unavailable_reason: undefined };
    vi.mocked(api.listProviders).mockResolvedValue([option, refreshedRow]);
    vi.mocked(api.saveSpawnConfiguration).mockResolvedValue(saved);
    render(<GroupedProviderMenu providers={[option, unavailableRow]} onSelect={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Codex configurations', exact: true }));
    await screen.findByLabelText('Edit Sol Max');
    const recipe = screen.getByText('Sol Max').closest('button[role="menuitem"]')!;
    expect(recipe.getAttribute('aria-disabled')).toBe('true');
    fireEvent.click(screen.getByRole('menuitem', { name: 'Edit Sol Max' }));
    await screen.findByRole('option', { name: 'Codex' });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));

    await screen.findByLabelText('Edit Sol Max');
    expect(screen.getByText('Sol Max').closest('button[role="menuitem"]')?.getAttribute('aria-disabled')).toBe('false');
  });

  it('tracks provider availability changes while the submenu stays open', async () => {
    const unavailableRow = { ...savedRow, unavailable_reason: 'Harness is unavailable' };
    const refreshedRow = { ...savedRow, unavailable_reason: undefined };
    const { rerender } = render(<GroupedProviderMenu providers={[option, unavailableRow]} onSelect={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Codex configurations', exact: true }));
    const recipe = await screen.findByText('Sol Max');
    expect(recipe.closest('button[role="menuitem"]')?.getAttribute('aria-disabled')).toBe('true');

    rerender(<GroupedProviderMenu providers={[option, refreshedRow]} onSelect={vi.fn()} />);

    await waitFor(() => expect(screen.getByText('Sol Max').closest('button[role="menuitem"]')?.getAttribute('aria-disabled')).toBe('false'));
  });

  it('does not let a pending save close a newer draft after Escape', async () => {
    let resolveSave!: (value: SpawnConfiguration) => void;
    vi.mocked(api.saveSpawnConfiguration).mockReturnValueOnce(new Promise((resolve) => { resolveSave = resolve; }));
    render(<GroupedProviderMenu providers={[option]} onSelect={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Codex configurations', exact: true }));
    fireEvent.click(await screen.findByRole('menuitem', { name: 'Edit Sol Max' }));
    await screen.findByRole('option', { name: 'Codex' });
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'Older edit' } });
    fireEvent.click(screen.getByRole('button', { name: 'Save' }));
    await waitFor(() => expect(api.saveSpawnConfiguration).toHaveBeenCalled());

    fireEvent.keyDown(screen.getByLabelText('Name'), { key: 'Escape' });
    fireEvent.click(await screen.findByRole('menuitem', { name: /New configuration/ }));
    fireEvent.change(screen.getByLabelText('Name'), { target: { value: 'Keep this draft' } });

    await act(async () => {
      resolveSave({ ...saved, name: 'Older edit' });
      await Promise.resolve();
    });

    expect((screen.getByLabelText('Name') as HTMLInputElement).value).toBe('Keep this draft');
    expect(screen.getByLabelText('Edit spawn configuration')).toBeTruthy();
  });

  it('reports launch-target load failures while editing a saved recipe', async () => {
    vi.mocked(api.getLaunchTargets).mockRejectedValueOnce(new Error('targets unavailable'));
    render(<GroupedProviderMenu providers={[option]} onSelect={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Codex configurations', exact: true }));
    fireEvent.click(await screen.findByRole('menuitem', { name: 'Edit Sol Max' }));

    expect((await screen.findByRole('alert')).textContent).toContain('targets unavailable');
    expect(screen.getByLabelText('Name')).toBeTruthy();
  });

  it('ignores a launch-target response after its editor is cancelled and reopened', async () => {
    let resolveFirst!: (value: typeof target[]) => void;
    let resolveSecond!: (value: typeof target[]) => void;
    vi.mocked(api.getLaunchTargets)
      .mockReturnValueOnce(new Promise((resolve) => { resolveFirst = resolve; }))
      .mockReturnValueOnce(new Promise((resolve) => { resolveSecond = resolve; }));
    render(<GroupedProviderMenu providers={[option]} onSelect={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Codex configurations', exact: true }));
    fireEvent.click(await screen.findByRole('menuitem', { name: 'Edit Sol Max' }));
    fireEvent.click(screen.getByRole('button', { name: 'Cancel' }));
    fireEvent.click(await screen.findByRole('menuitem', { name: 'Edit Sol Max' }));

    await act(async () => {
      resolveSecond([target, claudeTarget]);
      await Promise.resolve();
    });
    await act(async () => {
      resolveFirst([{ ...target, id: 'ghost', harness_id: 'ghost', harness_name: 'Ghost Harness' }]);
      await Promise.resolve();
    });

    const harnesses = screen.getByLabelText('Harness') as HTMLSelectElement;
    expect(Array.from(harnesses.options).map((entry) => entry.value)).toContain('claude');
    expect(Array.from(harnesses.options).map((entry) => entry.value)).not.toContain('ghost');
  });

  it('does not expose effort or extra arguments when unsupported', async () => {
    const limited = { ...option, capabilities: { ...option.capabilities!, supports_effort_override: false, supports_extra_args: false, effort_control: { kind: 'none' as const } } };
    render(<GroupedProviderMenu providers={[limited]} onSelect={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Codex configurations', exact: true }));
    await waitFor(() => expect((screen.getByRole('menuitem', { name: /New configuration/ }) as HTMLButtonElement).disabled).toBe(false));
    fireEvent.click(screen.getByRole('menuitem', { name: /New configuration/ }));
    expect(screen.getByLabelText('Model')).toBeTruthy();
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
