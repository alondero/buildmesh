import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen } from '@testing-library/react';
import { CommandOmnibar } from '../../src/components/CommandOmnibar/CommandOmnibar';
import { CompanionMode } from '../../src/components/CommandOmnibar/CompanionMode';
import { initializeCompanion, useCompanionStore } from '../../src/stores/companionStore';
import { useToastStore } from '../../src/stores/toastStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import { useUIStore } from '../../src/stores/uiStore';
import type { CompanionTask } from '../../src/types/generated/CompanionTask';

vi.mock('../../src/components/CommandOmnibar/omnibarActions', () => ({ loadSpawnOptions: async () => [], executeOmnibarItem: vi.fn() }));
const events = vi.hoisted(() => new Map<string, (event: { payload: CompanionTask }) => void>());
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async (name, callback) => { events.set(name, callback); return () => {}; }) }));
vi.mock('../../src/lib/tauri/companion', () => ({
  getCompanionTask: vi.fn(async () => null),
  cancelCompanionTask: vi.fn(async () => {}),
  respondCompanionConfirmation: vi.fn(async () => {}),
}));
const task = (overrides: Partial<CompanionTask> = {}): CompanionTask => ({
  id: 'one', generation: 1, revision: 1, prompt: 'status', status: 'running', steps: ['Read fleet'],
  confirmation: null, response: null, error: null, navigation: null, ...overrides,
});

beforeEach(() => {
  useCompanionStore.setState({ task: null, visible: false, starting: false, error: null });
  useUIStore.getState().openOmnibar('files');
  Element.prototype.scrollIntoView = vi.fn();
});

describe('Companion Omnibar', () => {
  it.each(['?', 'Tab'])('enters Companion with %s', key => {
    render(<CommandOmnibar />);
    const input = screen.getByRole('combobox');
    if (key === '?') fireEvent.change(input, { target: { value: '?' } });
    else fireEvent.keyDown(input, { key });
    expect(screen.getByRole('textbox', { name: 'Ask Companion' })).toBeTruthy();
  });

  it('offers natural language as the active keyboard option', () => {
    render(<CommandOmnibar />);
    fireEvent.change(screen.getByRole('combobox'), { target: { value: 'which agents need my help' } });
    const option = screen.getByRole('option', { name: /Ask Companion/ });
    expect(option.getAttribute('aria-selected')).toBe('true');
  });

  it('does not start inference for an unmatched scoped command', () => {
    render(<CommandOmnibar />);
    const input = screen.getByRole('combobox');
    fireEvent.change(input, { target: { value: '>nonexistent-command-xyz' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    expect(useCompanionStore.getState().visible).toBe(false);
    expect(useCompanionStore.getState().starting).toBe(false);
    expect(screen.queryByText(/Ask Companion/)).toBeNull();
  });

  it('closing and reopening preserves the running task', () => {
    useCompanionStore.setState({ task: task(), visible: true });
    render(<CommandOmnibar />);
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(useCompanionStore.getState().task?.status).toBe('running');
    act(() => useUIStore.getState().openOmnibar('files'));
    expect(screen.getByText('Read fleet')).toBeTruthy();
  });

  it('focuses confirmation and rejects on Escape without dismissing', async () => {
    const api = await import('../../src/lib/tauri/companion');
    useCompanionStore.setState({ task: task({ status: 'confirming', confirmation: 'Stop node 4?' }), visible: true });
    render(<CommandOmnibar />);
    expect(document.activeElement).toBe(screen.getByRole('button', { name: /Confirm execution/ }));
    fireEvent.keyDown(document, { key: 'Escape' });
    expect(api.respondCompanionConfirmation).toHaveBeenCalledWith('one', 1, false);
    expect(screen.getByRole('dialog')).toBeTruthy();
  });

  it('renders final Markdown and an actionable grid link', async () => {
    useCompanionStore.setState({ task: task({ status: 'completed', response: '**Done**. [Open Mesh Grid](buildmesh://grid)' }) });
    render(<CompanionMode initialPrompt="" onBack={() => {}} />);
    expect((await screen.findByText('Done')).tagName).toBe('STRONG');
    fireEvent.click(screen.getByRole('button', { name: 'Open Mesh Grid' }));
    expect(useUIStore.getState().viewMode).toBe('mesh');
    expect(useUIStore.getState().omnibarOpen).toBe(false);
  });
});

describe('Companion snapshot authority', () => {
  it('does not toast a rejected completion from a retired task', async () => {
    await initializeCompanion();
    useCompanionStore.getState().adopt(task({ id: 'new', generation: 2 }));
    useToastStore.setState({ toasts: [] });
    events.get('companion://completed')!({ payload: task({ status: 'completed' }) });
    expect(useToastStore.getState().toasts).toHaveLength(0);
    events.get('companion://completed')!({ payload: task({ id: 'new', generation: 2, revision: 2, status: 'completed' }) });
    expect(useToastStore.getState().toasts).toHaveLength(1);
  });

  it('opens a node link in its own mesh from another mesh', async () => {
    seedAgentNodes([{ id: 900, mesh_id: 42, name: 'impl', path: '/repo', branch: 'main', env: null,
      provider: 'claude', status: 'running', use_worktree: false, position: 0,
      created_at: '2026-01-01', scratchpad: '', sandbox: false, is_pinned: false }]);
    useMeshStore.setState({ selectedMeshId: 7 });
    useUIStore.setState({ viewMode: 'mesh' });
    useCompanionStore.setState({ task: task({ status: 'completed', response: '[Open node](buildmesh://node/900)' }) });
    render(<CompanionMode initialPrompt="" onBack={() => {}} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Open node' }));
    expect(useMeshStore.getState().selectedMeshId).toBe(42);
    expect(useUIStore.getState().viewMode).toBe('single');
  });

  it('drops older event revisions and retired-task events', () => {
    const adopt = useCompanionStore.getState().adopt;
    adopt(task({ revision: 4, status: 'completed' }));
    adopt(task({ revision: 2 }));
    expect(useCompanionStore.getState().task?.status).toBe('completed');
    adopt(task({ id: 'two', generation: 2, revision: 0 }));
    adopt(task({ revision: 9, status: 'error' }));
    expect(useCompanionStore.getState().task?.id).toBe('two');
  });
});
