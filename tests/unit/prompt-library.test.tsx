import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { PromptLibrary } from '../../src/components/Prompts/PromptLibrary';
import { BUILT_IN_PROMPTS } from '../../src/lib/promptLibrary';
import type { SavedPrompt } from '../../src/types/generated/SavedPrompt';
import type { SpawnOption } from '../../src/lib/groups';
import * as preferences from '../../src/lib/tauri/preferences';

vi.mock('../../src/lib/tauri/preferences', () => ({ listSavedPrompts: vi.fn(), savePrompt: vi.fn(), deleteSavedPrompt: vi.fn() }));
vi.mock('../../src/lib/tauri/provider', () => ({ getDefaultProvider: vi.fn().mockResolvedValue('claude') }));
vi.mock('../../src/components/Sidebar/SpawnButtonCluster', () => ({
  __resetDefaultChangedListeningForTests: () => undefined,
  SpawnButtonCluster: ({ onSpawnDefault, onSelectProvider, disabled, providers }: {
    onSpawnDefault: (alt: boolean) => void; onSelectProvider: (id: string, alt: boolean, configuration?: string, login?: string) => void;
    disabled: boolean; providers: SpawnOption[];
  }) => <div>
    <button disabled={disabled} onClick={() => onSpawnDefault(false)}>Spawn</button>
    <button disabled={disabled} onClick={() => onSelectProvider('codex', true, 'launch/fast', 'work')}>Spawn with configuration</button>
    <span data-testid="prompt-provider-ids">{providers.map(provider => provider.id).join(',')}</span>
  </div>,
}));

const SAVED: SavedPrompt = { id: 'saved-1', name: 'My review', body: 'Check the branch for regressions' };
const PROVIDERS = [
  { id: 'claude', harness_id: 'claude' }, { id: 'terminal', harness_id: 'terminal' },
] as SpawnOption[];

function setup(initialPrompt?: string) {
  const onSpawn = vi.fn().mockResolvedValue(undefined);
  const onDirtyChange = vi.fn();
  const result = render(<PromptLibrary meshId={7} providers={PROVIDERS} initialPrompt={initialPrompt} onSpawn={onSpawn} onDirtyChange={onDirtyChange} />);
  return { ...result, onSpawn, onDirtyChange };
}

beforeEach(() => {
  vi.clearAllMocks();
  vi.mocked(preferences.listSavedPrompts).mockResolvedValue([SAVED]);
});

describe('prompt library', () => {
  it('searches names and full prompt text and previews a built-in without launching', async () => {
    const { onSpawn } = setup();
    await screen.findByRole('button', { name: SAVED.name });
    fireEvent.change(screen.getByLabelText('Search prompts'), { target: { value: 'codebase architecture' } });
    expect(screen.queryByRole('button', { name: SAVED.name })).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Explore this project' }));
    expect((screen.getByLabelText('Initial prompt') as HTMLTextAreaElement).value).toBe(BUILT_IN_PROMPTS[2].body);
    expect(onSpawn).not.toHaveBeenCalled();
    fireEvent.change(screen.getByLabelText('Search prompts'), { target: { value: 'nothing-matches' } });
    expect(screen.getByText('No prompts match your search.')).toBeTruthy();
  });

  it('saves built-ins as copies and edits saved prompts by stable id', async () => {
    setup();
    await screen.findByRole('button', { name: SAVED.name });
    fireEvent.click(screen.getByRole('button', { name: 'Explore this project' }));
    const copy = { ...BUILT_IN_PROMPTS[2], id: 'copy-id' };
    vi.mocked(preferences.savePrompt).mockResolvedValue(copy);
    fireEvent.click(screen.getByRole('button', { name: 'Save a copy' }));
    await waitFor(() => expect(preferences.savePrompt).toHaveBeenCalledWith(null, copy.name, copy.body));
    await screen.findByRole('button', { name: 'Save changes' });
    fireEvent.change(screen.getByLabelText('Initial prompt'), { target: { value: 'Personalized' } });
    vi.mocked(preferences.savePrompt).mockResolvedValue({ ...copy, body: 'Personalized' });
    fireEvent.click(screen.getByRole('button', { name: 'Save changes' }));
    await waitFor(() => expect(preferences.savePrompt).toHaveBeenLastCalledWith('copy-id', copy.name, 'Personalized'));
  });

  it('keeps a failed save draft for retry and protects it when selecting another prompt', async () => {
    const { onDirtyChange } = setup();
    await screen.findByRole('button', { name: SAVED.name });
    fireEvent.change(screen.getByLabelText('Prompt name'), { target: { value: 'New prompt' } });
    fireEvent.change(screen.getByLabelText('Initial prompt'), { target: { value: 'Keep my input' } });
    vi.mocked(preferences.savePrompt).mockRejectedValue(new Error('Disk full'));
    fireEvent.click(screen.getByRole('button', { name: 'Save prompt' }));
    expect((await screen.findByRole('alert')).textContent).toContain('Disk full');
    expect((screen.getByLabelText('Initial prompt') as HTMLTextAreaElement).value).toBe('Keep my input');
    expect(onDirtyChange).toHaveBeenLastCalledWith(true);
    fireEvent.click(screen.getByRole('button', { name: SAVED.name }));
    fireEvent.click(screen.getByRole('button', { name: 'Keep editing' }));
    expect((screen.getByLabelText('Initial prompt') as HTMLTextAreaElement).value).toBe('Keep my input');
    vi.mocked(preferences.savePrompt).mockResolvedValue({ id: 'new', name: 'New prompt', body: 'Keep my input' });
    fireEvent.click(screen.getByRole('button', { name: 'Save prompt' }));
    await waitFor(() => expect(onDirtyChange).toHaveBeenLastCalledWith(false));
  });

  it('confirms deletion and leaves the saved prompt intact when deletion fails', async () => {
    setup();
    fireEvent.click(await screen.findByRole('button', { name: SAVED.name }));
    fireEvent.click(screen.getByRole('button', { name: 'Delete prompt' }));
    expect(preferences.deleteSavedPrompt).not.toHaveBeenCalled();
    vi.mocked(preferences.deleteSavedPrompt).mockRejectedValueOnce(new Error('Read only')).mockResolvedValueOnce(undefined);
    fireEvent.click(screen.getByRole('button', { name: 'Confirm delete' }));
    await screen.findByText('Read only');
    expect(screen.getByRole('button', { name: SAVED.name })).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Delete prompt' }));
    fireEvent.click(screen.getByRole('button', { name: 'Confirm delete' }));
    await waitFor(() => expect(screen.queryByRole('button', { name: SAVED.name })).toBeNull());
  });

  it('sends a one-off edited prompt with configuration, login and worktree modifier, without saving', async () => {
    const { onSpawn } = setup();
    fireEvent.click(await screen.findByRole('button', { name: SAVED.name }));
    fireEvent.change(screen.getByLabelText('Initial prompt'), { target: { value: '  One-off instructions  ' } });
    fireEvent.click(screen.getByRole('button', { name: 'Spawn with configuration' }));
    await waitFor(() => expect(onSpawn).toHaveBeenCalledWith('One-off instructions', 'codex', true, 'launch/fast', 'work'));
    expect(preferences.savePrompt).not.toHaveBeenCalled();
    expect(screen.getByTestId('prompt-provider-ids').textContent).toBe('claude');
  });

  it('disables launch while in flight and retries after a rejected create', async () => {
    const { onSpawn } = setup('Initial instructions');
    let reject!: (error: Error) => void;
    onSpawn.mockImplementationOnce(() => new Promise<void>((_, fail) => { reject = fail; }));
    fireEvent.click(screen.getByRole('button', { name: 'Spawn' }));
    await waitFor(() => expect(onSpawn).toHaveBeenCalledTimes(1));
    expect((screen.getByRole('button', { name: 'Spawn' }) as HTMLButtonElement).disabled).toBe(true);
    reject(new Error('Cannot create agent'));
    await screen.findByText('Cannot create agent');
    fireEvent.click(screen.getByRole('button', { name: 'Spawn' }));
    await waitFor(() => expect(onSpawn).toHaveBeenCalledTimes(2));
  });

  it('retries loading and does not overwrite library data before it loads', async () => {
    vi.mocked(preferences.listSavedPrompts).mockRejectedValueOnce(new Error('Offline'));
    setup();
    await screen.findByRole('alert');
    fireEvent.click(screen.getByRole('button', { name: 'Explore this project' }));
    expect((screen.getByRole('button', { name: 'Save a copy' }) as HTMLButtonElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: 'Retry' }));
    await screen.findByRole('button', { name: SAVED.name });
    expect((screen.getByRole('button', { name: 'Save a copy' }) as HTMLButtonElement).disabled).toBe(false);
  });
});
