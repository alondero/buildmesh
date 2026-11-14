import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { PromptLibrary } from '../../src/components/Prompts/PromptLibrary';
import { BUILT_IN_PROMPTS } from '../../src/lib/promptLibrary';
import type { SavedPrompt } from '../../src/types/generated/SavedPrompt';
import type { SpawnOption } from '../../src/lib/groups';
import type { Mesh } from '../../src/types/generated/Mesh';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import * as preferences from '../../src/lib/tauri/preferences';
import { getDefaultProvider } from '../../src/lib/tauri/provider';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';

vi.mock('../../src/lib/tauri/preferences', () => ({ listSavedPrompts: vi.fn(), savePrompt: vi.fn(), deleteSavedPrompt: vi.fn() }));
vi.mock('../../src/lib/tauri/provider', () => ({ getDefaultProvider: vi.fn() }));
vi.mock('../../src/components/Sidebar/SpawnButtonCluster', () => ({
  __resetDefaultChangedListeningForTests: () => undefined,
  SpawnButtonCluster: ({ onSpawnDefault, onSelectProvider, disabled, isSpawning, primaryAriaLabel, providers }: {
    onSpawnDefault: (alt: boolean) => void;
    onSelectProvider: (id: string, alt: boolean, configuration?: string, login?: string) => void;
    disabled: boolean; isSpawning: boolean; primaryAriaLabel: string; providers: SpawnOption[];
  }) => <div>
    <button disabled={disabled || isSpawning} aria-label={primaryAriaLabel} onClick={() => onSpawnDefault(false)}>Spawn</button>
    <button disabled={disabled || isSpawning} onClick={() => onSelectProvider('codex', true, 'launch/fast', 'work')}>Spawn with configuration</button>
    <span data-testid="prompt-provider-ids">{providers.map(provider => provider.id).join(',')}</span>
  </div>,
}));

const SAVED: SavedPrompt = { id: 'saved-1', name: 'My project review', body: 'Check project workflows for regressions' };
const MESH = { id: 7, name: 'Target Mesh', path: '/project', use_worktree: true, base_ref: 'origin/develop' } as Mesh;
const OTHER = { ...MESH, id: 8, name: 'Other Mesh', path: '/other', base_ref: 'origin/main' };
const NODE = { id: 42, mesh_id: 7, name: 'fresh-agent', provider: 'codex', status: 'idle' } as AgentNode;
const PROVIDERS = [{ id: 'claude', harness_id: 'claude' }, { id: 'codex', harness_id: 'codex' }, { id: 'terminal', harness_id: 'terminal' }] as SpawnOption[];
const create = vi.fn();
const spawnAgent = vi.fn();

function setup(mesh: Mesh | null = MESH) {
  return render(<PromptLibrary mesh={mesh} providers={PROVIDERS} />);
}
function row(id = SAVED.id) { return document.querySelector<HTMLElement>(`[data-prompt-row="${id}"]`)!; }
async function action(name: string, promptName = SAVED.name) {
  fireEvent.click(await screen.findByRole('button', { name: `Actions for ${promptName}` }));
  fireEvent.click(screen.getByRole('menuitem', { name }));
}
function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (reason: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

beforeEach(() => {
  vi.clearAllMocks();
  localStorage.removeItem('buildmesh.prompt-spawn-mesh');
  vi.mocked(preferences.listSavedPrompts).mockResolvedValue([SAVED]);
  vi.mocked(getDefaultProvider).mockResolvedValue('claude');
  create.mockImplementation(async () => {
    useAgentNodeStore.setState(state => ({ nodesById: { ...state.nodesById, [NODE.id]: NODE } }));
    return NODE;
  });
  spawnAgent.mockResolvedValue(undefined);
  useMeshStore.setState({ selectedMeshId: MESH.id, meshes: [MESH, OTHER], meshesById: new Map([[7, MESH], [8, OTHER]]) });
  useAgentNodeStore.setState({ createAgentNode: create, spawnAgent, activeNodeId: 99, nodesById: {
    99: { ...NODE, id: 99, path: '/another-agent-checkout', mesh_id: OTHER.id },
  } });
});

describe('Prompts Probe library', () => {
  it('defaults to the only Mesh with no active nodes or selection', async () => {
    useMeshStore.setState({ meshes: [MESH], meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: null });
    useAgentNodeStore.setState({ nodesById: {}, activeNodeId: null });
    useUIStore.getState().setViewMode('all');
    setup(null);
    expect((screen.getByLabelText('Spawn in Mesh') as HTMLSelectElement).value).toBe('7');
    fireEvent.click(await screen.findByRole('button', { name: `Spawn ${SAVED.name}` }));
    await waitFor(() => expect(create).toHaveBeenCalledWith(7, MESH.name, MESH.path, 'main', 'claude', true, undefined, SAVED.body, undefined));
    expect(useUIStore.getState().viewMode).toBe('all');
    expect(useMeshStore.getState().selectedMeshId).toBeNull();
  });

  it.each(['all', 'pinned'] as const)('changes the inline target without changing %s canvas scope', async mode => {
    useUIStore.getState().setViewMode(mode);
    const selected = useMeshStore.getState().selectedMeshId;
    setup();
    fireEvent.change(screen.getByLabelText('Spawn in Mesh'), { target: { value: '8' } });
    fireEvent.click(await screen.findByRole('button', { name: `Spawn ${SAVED.name}` }));
    await waitFor(() => expect(create).toHaveBeenCalledWith(8, OTHER.name, OTHER.path, 'main', 'claude', true, undefined, SAVED.body, undefined));
    expect(useUIStore.getState().viewMode).toBe(mode);
    expect(useMeshStore.getState().selectedMeshId).toBe(selected);
  });

  it('asks for a Mesh on quick spawn and retains the chosen provider, configuration and login', async () => {
    setup(null);
    await screen.findByRole('button', { name: SAVED.name });
    fireEvent.click(within(row()).getByRole('button', { name: 'Spawn with configuration' }));
    expect(create).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('menuitem', { name: OTHER.name }));
    await waitFor(() => expect(create).toHaveBeenCalledWith(8, OTHER.name, OTHER.path, 'main', 'codex', true, 'launch/fast', SAVED.body, 'work'));
  });

  it('asks for a Mesh from primary Spawn and resolves that Mesh default provider', async () => {
    setup(null);
    fireEvent.click(await screen.findByRole('button', { name: `Spawn ${SAVED.name}` }));
    expect(create).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('menuitem', { name: OTHER.name }));
    await waitFor(() => expect(create).toHaveBeenCalledWith(8, OTHER.name, OTHER.path, 'main', 'claude', true, undefined, SAVED.body, undefined));
    expect(getDefaultProvider).toHaveBeenCalledWith(OTHER.id);
  });

  it('wraps the complete launch base ref instead of truncating its meaningful tail', async () => {
    const mesh = { ...MESH, base_ref: `origin/${'long-branch-'.repeat(20)}meaningful-tail` };
    useMeshStore.setState({ meshes: [mesh], meshesById: new Map([[mesh.id, mesh]]) });
    setup(mesh);
    await screen.findByRole('button', { name: SAVED.name });
    const location = screen.getByText(`· from ${mesh.base_ref}`);
    expect(location.className).toContain('break-words');
    expect(location.className).not.toContain('truncate');
  });

  it('honours a Mesh picked from an unresolved custom launch Spawn button', async () => {
    setup(null);
    fireEvent.click(await screen.findByRole('button', { name: SAVED.name }));
    fireEvent.click(within(row()).getByRole('button', { name: 'Customise for this launch' }));
    const dialog = within(screen.getByRole('dialog'));
    fireEvent.change(dialog.getByLabelText('Prompt for this launch'), { target: { value: 'Custom draft' } });
    fireEvent.click(dialog.getByRole('button', { name: `Spawn ${SAVED.name}` }));
    fireEvent.click(screen.getByRole('menuitem', { name: OTHER.name }));
    await waitFor(() => expect(create).toHaveBeenCalledWith(8, OTHER.name, OTHER.path, 'main', 'claude', true, undefined, 'Custom draft', undefined));
  });

  it('lets an unresolved custom launch select a Mesh before spawning', async () => {
    setup(null);
    fireEvent.click(await screen.findByRole('button', { name: SAVED.name }));
    fireEvent.click(within(row()).getByRole('button', { name: 'Customise for this launch' }));
    const dialog = within(screen.getByRole('dialog'));
    fireEvent.change(dialog.getByLabelText('Spawn in Mesh'), { target: { value: '8' } });
    fireEvent.change(dialog.getByLabelText('Prompt for this launch'), { target: { value: 'One launch only' } });
    fireEvent.click(dialog.getByRole('button', { name: 'Spawn with configuration' }));
    await waitFor(() => expect(create).toHaveBeenCalledWith(8, OTHER.name, OTHER.path, 'main', 'codex', true, 'launch/fast', 'One launch only', 'work'));
  });

  it('restores a valid last-used spawn target and follows explicit Mesh changes', async () => {
    localStorage.setItem('buildmesh.prompt-spawn-mesh', '8');
    const view = setup(null);
    expect((screen.getByLabelText('Spawn in Mesh') as HTMLSelectElement).value).toBe('8');
    view.rerender(<PromptLibrary mesh={MESH} providers={PROVIDERS} />);
    expect((screen.getByLabelText('Spawn in Mesh') as HTMLSelectElement).value).toBe('7');
    fireEvent.change(screen.getByLabelText('Spawn in Mesh'), { target: { value: '8' } });
    view.rerender(<PromptLibrary mesh={OTHER} providers={PROVIDERS} />);
    view.rerender(<PromptLibrary mesh={MESH} providers={PROVIDERS} />);
    expect((screen.getByLabelText('Spawn in Mesh') as HTMLSelectElement).value).toBe('7');
  });

  it('ignores a deleted last-used Mesh and disables launch when no Meshes exist', async () => {
    localStorage.setItem('buildmesh.prompt-spawn-mesh', '999');
    setup(null);
    expect((screen.getByLabelText('Spawn in Mesh') as HTMLSelectElement).value).toBe('');
    await screen.findByRole('button', { name: SAVED.name });
    act(() => useMeshStore.setState({ meshes: [], meshesById: new Map() }));
    expect((screen.getByRole('button', { name: `Spawn ${SAVED.name}` }) as HTMLButtonElement).disabled).toBe(true);
  });

  it('follows a newly focused agent or sidebar selection even within the same inspection Mesh', async () => {
    useMeshStore.setState({ selectedMeshId: null });
    setup();
    await screen.findByRole('button', { name: SAVED.name });
    fireEvent.change(screen.getByLabelText('Spawn in Mesh'), { target: { value: '8' } });
    act(() => useAgentNodeStore.setState({ activeNodeId: NODE.id, nodesById: { [NODE.id]: NODE } }));
    expect((screen.getByLabelText('Spawn in Mesh') as HTMLSelectElement).value).toBe('7');
    fireEvent.change(screen.getByLabelText('Spawn in Mesh'), { target: { value: '8' } });
    act(() => useMeshStore.setState({ selectedMeshId: MESH.id }));
    expect((screen.getByLabelText('Spawn in Mesh') as HTMLSelectElement).value).toBe('7');
  });

  it('browses read-only, searches full text and expands a selectable prompt without launching', async () => {
    setup();
    await screen.findByRole('button', { name: SAVED.name });
    expect(screen.queryByRole('textbox', { name: 'Prompt' })).toBeNull();
    fireEvent.change(screen.getByLabelText('Search prompts'), { target: { value: 'codebase architecture' } });
    expect(screen.queryByRole('button', { name: SAVED.name })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Explore this project' }));
    const body = document.querySelector('[data-prompt-body-expanded]')!;
    expect(body.textContent).toBe(BUILT_IN_PROMPTS[2].body);
    expect(body.getAttribute('role')).toBeNull();
    fireEvent.click(body);
    expect(document.querySelector('[data-prompt-body-expanded]')).toBe(body);
    expect(create).not.toHaveBeenCalled();
    fireEvent.change(screen.getByLabelText('Search prompts'), { target: { value: 'nothing-matches' } });
    expect(screen.getByText('No prompts match your search.')).toBeTruthy();
  });

  it('launches a fresh agent with Mesh defaults and preserves the focused agent and Mesh', async () => {
    setup();
    fireEvent.click(await screen.findByRole('button', { name: `Spawn ${SAVED.name}` }));
    await waitFor(() => expect(spawnAgent).toHaveBeenCalledWith(42, 'codex', { prefill: SAVED.body }));
    expect(create).toHaveBeenCalledWith(7, MESH.name, MESH.path, 'main', 'claude', true, undefined, SAVED.body, undefined);
    expect(screen.getByTestId('prompt-launch-context').textContent).toContain('from origin/develop');
    expect((screen.getByLabelText('Spawn in Mesh') as HTMLSelectElement).value).toBe('7');
    expect(useAgentNodeStore.getState().activeNodeId).toBe(99);
    expect(useMeshStore.getState().selectedMeshId).toBe(7);
    expect(screen.getByText(/Started fresh-agent in Target Mesh/)).toBeTruthy();
    expect(screen.getByTestId('prompt-library')).toBeTruthy();
  });

  it('passes configuration and login while always honouring Mesh worktree settings', async () => {
    setup();
    await screen.findByRole('button', { name: SAVED.name });
    fireEvent.click(within(row()).getByRole('button', { name: 'Spawn with configuration' }));
    await waitFor(() => expect(create).toHaveBeenCalledWith(7, MESH.name, MESH.path, 'main', 'codex', true, 'launch/fast', SAVED.body, 'work'));
    expect(within(row()).getByTestId('prompt-provider-ids').textContent).toBe('claude,codex');
  });

  it('keeps the clicked Mesh during delayed default resolution without changing the current selection', async () => {
    const pending = deferred<string>();
    vi.mocked(getDefaultProvider).mockReturnValue(pending.promise);
    const view = setup();
    fireEvent.click(await screen.findByRole('button', { name: `Spawn ${SAVED.name}` }));
    expect((within(row()).getByRole('button', { name: `Spawn ${SAVED.name}` }) as HTMLButtonElement).disabled).toBe(true);
    act(() => useMeshStore.setState({ selectedMeshId: OTHER.id }));
    view.rerender(<PromptLibrary mesh={OTHER} providers={PROVIDERS} />);
    await act(async () => pending.resolve('claude'));
    await waitFor(() => expect(create).toHaveBeenCalledWith(7, MESH.name, MESH.path, 'main', 'claude', true, undefined, SAVED.body, undefined));
    expect(useMeshStore.getState().selectedMeshId).toBe(OTHER.id);
    expect(preferences.listSavedPrompts).toHaveBeenCalledTimes(1);
  });

  it('allows creating prompts without a Mesh or agent installation while disabling spawn', async () => {
    render(<PromptLibrary mesh={null} providers={[]} />);
    await screen.findByRole('button', { name: SAVED.name });
    expect(screen.getByLabelText('Spawn in Mesh')).toBeTruthy();
    expect((screen.getByRole('button', { name: `Spawn ${SAVED.name}` }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: 'New prompt' }));
    fireEvent.change(screen.getByLabelText('Title'), { target: { value: 'A reusable task' } });
    fireEvent.change(screen.getByLabelText('Prompt'), { target: { value: 'Read the instructions' } });
    vi.mocked(preferences.savePrompt).mockResolvedValue({ id: 'new', name: 'A reusable task', body: 'Read the instructions' });
    fireEvent.click(screen.getByRole('button', { name: 'Save prompt' }));
    await screen.findByRole('button', { name: 'A reusable task' });
    expect(preferences.savePrompt).toHaveBeenCalledWith(null, 'A reusable task', 'Read the instructions');
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('duplicates a built-in and edits saved prompts using their stable id', async () => {
    setup();
    await action('Duplicate', BUILT_IN_PROMPTS[2].name);
    expect((screen.getByLabelText('Title') as HTMLInputElement).value).toBe('Explore this project (copy)');
    vi.mocked(preferences.savePrompt).mockResolvedValue({ ...BUILT_IN_PROMPTS[2], id: 'copy', name: 'Explore this project (copy)' });
    fireEvent.click(screen.getByRole('button', { name: 'Save prompt' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
    expect(preferences.savePrompt).toHaveBeenCalledWith(null, 'Explore this project (copy)', BUILT_IN_PROMPTS[2].body);
    await action('Edit saved prompt');
    fireEvent.change(screen.getByLabelText('Prompt'), { target: { value: 'Updated library instructions' } });
    vi.mocked(preferences.savePrompt).mockResolvedValue({ ...SAVED, body: 'Updated library instructions' });
    fireEvent.click(screen.getByRole('button', { name: 'Save changes' }));
    await waitFor(() => expect(preferences.savePrompt).toHaveBeenLastCalledWith(SAVED.id, SAVED.name, 'Updated library instructions'));
  });

  it('keeps failed-save drafts and protects them when closing the editor', async () => {
    setup();
    await action('Edit saved prompt');
    fireEvent.change(screen.getByLabelText('Prompt'), { target: { value: 'Keep this draft' } });
    vi.mocked(preferences.savePrompt).mockRejectedValueOnce(new Error('Disk full')).mockResolvedValueOnce({ ...SAVED, body: 'Keep this draft' });
    fireEvent.click(screen.getByRole('button', { name: 'Save changes' }));
    await screen.findByText('Disk full');
    fireEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(screen.getByText('Discard unsaved changes?')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Keep editing' }));
    expect((screen.getByLabelText('Prompt') as HTMLTextAreaElement).value).toBe('Keep this draft');
    fireEvent.click(screen.getByRole('button', { name: 'Save changes' }));
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());
  });

  it('customises a single launch without writing to the library', async () => {
    setup();
    fireEvent.click(await screen.findByRole('button', { name: SAVED.name }));
    fireEvent.click(within(row()).getByRole('button', { name: 'Customise for this launch' }));
    fireEvent.change(screen.getByLabelText('Prompt for this launch'), { target: { value: '  One-off instructions  ' } });
    fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: 'Spawn with configuration' }));
    await waitFor(() => expect(spawnAgent).toHaveBeenCalledWith(42, 'codex', { prefill: 'One-off instructions' }));
    expect(preferences.savePrompt).not.toHaveBeenCalled();
    expect(row().textContent).toContain(SAVED.body);
    expect(screen.queryByRole('dialog')).toBeNull();
  });

  it('retries a created node with an edited draft and the original configuration, without duplicate creation', async () => {
    spawnAgent.mockRejectedValueOnce(new Error('CLI not ready')).mockResolvedValueOnce(undefined);
    setup();
    fireEvent.click(await screen.findByRole('button', { name: `Spawn ${SAVED.name}` }));
    await screen.findByText('CLI not ready');
    fireEvent.change(screen.getByLabelText('Prompt for this launch'), { target: { value: 'Revised attempt' } });
    fireEvent.click(screen.getByRole('button', { name: 'Retry launch' }));
    await waitFor(() => expect(spawnAgent).toHaveBeenLastCalledWith(42, 'codex', { prefill: 'Revised attempt' }));
    expect(create).toHaveBeenCalledTimes(1);
    expect(preferences.savePrompt).not.toHaveBeenCalled();
  });

  it('keeps launch customisation dirty after a failed create and asks before discarding', async () => {
    create.mockRejectedValue(new Error('Create failed'));
    setup();
    fireEvent.click(await screen.findByRole('button', { name: SAVED.name }));
    fireEvent.click(within(row()).getByRole('button', { name: 'Customise for this launch' }));
    fireEvent.change(screen.getByLabelText('Prompt for this launch'), { target: { value: 'Do not lose this' } });
    fireEvent.click(within(screen.getByRole('dialog')).getByRole('button', { name: `Spawn ${SAVED.name}` }));
    await screen.findByText('Create failed');
    fireEvent.click(screen.getByRole('button', { name: 'Close' }));
    expect(screen.getByText('Discard unsaved changes?')).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Keep editing' }));
    expect((screen.getByLabelText('Prompt for this launch') as HTMLTextAreaElement).value).toBe('Do not lose this');
  });

  it('does not retry a failed node that was deleted or started elsewhere', async () => {
    spawnAgent.mockRejectedValueOnce(new Error('CLI not ready'));
    setup();
    fireEvent.click(await screen.findByRole('button', { name: `Spawn ${SAVED.name}` }));
    await screen.findByText('CLI not ready');
    act(() => useAgentNodeStore.setState(state => ({ nodesById: { ...state.nodesById, [NODE.id]: { ...NODE, status: 'running' } } })));
    fireEvent.click(screen.getByRole('button', { name: 'Retry launch' }));
    await screen.findByText(/This agent has changed since the failed launch/);
    act(() => useAgentNodeStore.setState(state => ({ nodesById: { ...state.nodesById, [NODE.id]: { ...NODE, status: 'archived' } } })));
    fireEvent.click(screen.getByRole('button', { name: 'Retry launch' }));
    await screen.findByText(/This agent is no longer available/);
    expect(spawnAgent).toHaveBeenCalledTimes(1);
    expect(create).toHaveBeenCalledTimes(1);
  });

  it('confirms deletion and retains the prompt after a rejected delete', async () => {
    setup();
    await action('Delete prompt');
    expect(preferences.deleteSavedPrompt).not.toHaveBeenCalled();
    vi.mocked(preferences.deleteSavedPrompt).mockRejectedValueOnce(new Error('Read only')).mockResolvedValueOnce(undefined);
    fireEvent.click(screen.getByRole('button', { name: 'Confirm delete' }));
    await screen.findByText('Read only');
    expect(row()).toBeTruthy();
    await action('Delete prompt');
    fireEvent.click(screen.getByRole('button', { name: 'Confirm delete' }));
    await waitFor(() => expect(row()).toBeNull());
  });

  it('retries library loading and prevents saving over unread data', async () => {
    vi.mocked(preferences.listSavedPrompts).mockRejectedValueOnce(new Error('Offline'));
    setup();
    await screen.findByRole('alert');
    expect((screen.getByRole('button', { name: 'New prompt' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    await screen.findByRole('button', { name: SAVED.name });
    expect((screen.getByRole('button', { name: 'New prompt' }) as HTMLButtonElement).disabled).toBe(false);
  });

  it('closes confirmation and disables actions during a pending deletion, then restores focus to search', async () => {
    const pending = deferred<void>();
    vi.mocked(preferences.deleteSavedPrompt).mockReturnValue(pending.promise);
    setup();
    await action('Delete prompt');
    fireEvent.click(screen.getByRole('button', { name: 'Confirm delete' }));
    expect(screen.queryByRole('dialog')).toBeNull();
    expect((within(row()).getByRole('button', { name: `Spawn ${SAVED.name}` }) as HTMLButtonElement).disabled).toBe(true);
    await act(async () => pending.resolve());
    await waitFor(() => expect(row()).toBeNull());
    expect(document.activeElement).toBe(screen.getByLabelText('Search prompts'));
  });

  it('does not create a node when the default agent cannot receive prompts', async () => {
    vi.mocked(getDefaultProvider).mockResolvedValue('terminal');
    setup();
    fireEvent.click(await screen.findByRole('button', { name: `Spawn ${SAVED.name}` }));
    await screen.findByText(/Choose an agent that supports an initial prompt/);
    expect(create).not.toHaveBeenCalled();
  });

  it('has one scroll owner with pinned toolbar and full-width wrapping prompt text', async () => {
    const longPrompt = { ...SAVED, body: 'Review this very long instruction '.repeat(100) };
    vi.mocked(preferences.listSavedPrompts).mockResolvedValue([longPrompt]);
    setup();
    fireEvent.click(await screen.findByRole('button', { name: SAVED.name }));
    const root = screen.getByTestId('prompt-library');
    expect(root.className).not.toContain('overflow-');
    expect(root.querySelectorAll('[class*="overflow-y-auto"]').length).toBe(1);
    const body = screen.getByTestId('prompt-library-body');
    expect(body.contains(screen.getByTestId('probe-toolbar'))).toBe(false);
    expect(document.querySelector('[data-prompt-body-expanded]')?.textContent).toBe(longPrompt.body);
  });
});
