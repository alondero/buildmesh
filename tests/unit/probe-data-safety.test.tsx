import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { ScratchpadTab } from '../../src/components/Probe/ScratchpadTab';
import { ProjectSettingsTab } from '../../src/components/Probe/ProjectSettingsTab';
import { RepositoryTab } from '../../src/components/Probe/RepositoryTab';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';
import { seedAgentNodes } from './helpers/seedAgentNodes';
import { getMeshScratchpad, setMeshScratchpad } from '../../src/lib/tauri';

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

let nextMeshId = 701;
let alpha: Mesh;
let beta: Mesh;
const config = (name: string) => ({ name, build_command: `${name}-build`, sandbox: false, use_worktree: true });
const pruneInfo = (path: string, name: string) => [{
  path,
  local_branches: [{ name, is_head: false, is_active: false, has_uncommitted: false,
    checked_out_in_worktree: false, is_merged_into_main: true, is_orphan: false }],
  worktrees: [], remote_tracking_branches: [],
}];

beforeEach(() => {
  alpha = { id: nextMeshId++, name: 'Alpha', path: '/repos/alpha', layout: 'grid', position: 0 } as Mesh;
  beta = { ...alpha, id: nextMeshId++, name: 'Beta', path: '/repos/beta' };
  seedAgentNodes([]);
  useMeshStore.setState({ meshes: [alpha, beta], meshesById: new Map([[alpha.id, alpha], [beta.id, beta]]), selectedMeshId: alpha.id });
  useUIStore.setState({ viewMode: 'mesh', probeContextPins: {} });
  vi.mocked(invoke).mockReset().mockImplementation(async (cmd, args) => {
    if (cmd === 'get_mesh_scratchpad') return `Notes for ${(args as { meshId: number }).meshId}`;
    if (cmd === 'get_mesh_properties') return config((args as { meshId: number }).meshId === alpha.id ? 'Alpha' : 'Beta');
    if (cmd === 'get_git_prune_info') return pruneInfo('/repos/alpha', 'alpha-done');
    if (cmd === 'get_mesh_health') return { is_drifted: false, is_dirty: false, unpushed_ahead: 0, base_branch_holder: null };
    if (cmd === 'list_providers') return [];
    if (cmd === 'get_mesh_pool_count') return 0;
    return {};
  });
});

afterEach(() => { cleanup(); vi.useRealTimers(); });

describe('Notes request ownership', () => {
  it('keeps the newest draft in Saving when an earlier write completes', async () => {
    vi.useFakeTimers();
    const write = deferred<void>();
    const normal = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation((cmd, args, options) => cmd === 'set_mesh_scratchpad' ? write.promise : normal(cmd, args, options));
    useUIStore.setState({ probeTab: 'scratchpad' });
    render(<ScratchpadTab />);
    await act(async () => {});
    fireEvent.change(screen.getByLabelText('Scratch pad'), { target: { value: 'first draft' } });
    await act(async () => { await vi.advanceTimersByTimeAsync(500); });
    fireEvent.change(screen.getByLabelText('Scratch pad'), { target: { value: 'newer draft' } });
    await act(async () => { write.resolve(); });
    expect(screen.getByText('Saving…')).toBeTruthy();
    expect(screen.queryByText('Saved')).toBeNull();
    await act(async () => { await vi.advanceTimersByTimeAsync(500); });
    expect(screen.getByText('Saved')).toBeTruthy();
  });

  it('blocks edits until the selected mesh has loaded', async () => {
    const read = deferred<string>();
    vi.mocked(invoke).mockImplementation((cmd) => cmd === 'get_mesh_scratchpad' ? read.promise : Promise.resolve());
    useUIStore.setState({ probeTab: 'scratchpad' });
    render(<ScratchpadTab />);
    expect((screen.getByLabelText('Scratch pad') as HTMLTextAreaElement).disabled).toBe(true);
    await act(async () => { read.resolve('Existing valuable notes'); });
    expect((screen.getByLabelText('Scratch pad') as HTMLTextAreaElement).value).toBe('Existing valuable notes');
    expect((screen.getByLabelText('Scratch pad') as HTMLTextAreaElement).disabled).toBe(false);
  });

  it('quarantines a failed read and retries without writing a blank replacement', async () => {
    vi.mocked(invoke).mockRejectedValueOnce(new Error('Notes unavailable'));
    useUIStore.setState({ probeTab: 'scratchpad' });
    render(<ScratchpadTab />);
    await screen.findByText('Load failed');
    expect((screen.getByLabelText('Scratch pad') as HTMLTextAreaElement).disabled).toBe(true);
    fireEvent.click(screen.getByRole('button', { name: /retry.*notes/i }));
    await waitFor(() => expect((screen.getByLabelText('Scratch pad') as HTMLTextAreaElement).value).toBe(`Notes for ${alpha.id}`));
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'set_mesh_scratchpad')).toHaveLength(0);
  });

  it('does not let an outgoing save failure overwrite the incoming mesh status', async () => {
    vi.useFakeTimers();
    const write = deferred<void>();
    const normal = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation((cmd, args, options) => cmd === 'set_mesh_scratchpad' ? write.promise : normal(cmd, args, options));
    useUIStore.setState({ probeTab: 'scratchpad' });
    render(<ScratchpadTab />);
    await act(async () => {});
    fireEvent.change(screen.getByLabelText('Scratch pad'), { target: { value: 'Alpha draft' } });
    await act(async () => { await vi.advanceTimersByTimeAsync(500); });
    await act(async () => { useMeshStore.getState().selectMesh(beta.id); });
    await act(async () => { write.reject(new Error('Alpha failed')); });
    expect((screen.getByLabelText('Scratch pad') as HTMLTextAreaElement).value).toBe(`Notes for ${beta.id}`);
    expect(screen.queryByText(/Save failed/)).toBeNull();
  });

  it('waits for an outgoing save before re-reading that mesh', async () => {
    vi.useFakeTimers();
    const write = deferred<void>();
    const normal = vi.mocked(invoke).getMockImplementation()!;
    let persisted = 'old Alpha notes';
    vi.mocked(invoke).mockImplementation((cmd, args, options) => {
      if (cmd === 'set_mesh_scratchpad') return write.promise.then(() => { persisted = (args as { content: string }).content; });
      if (cmd === 'get_mesh_scratchpad' && (args as { meshId: number }).meshId === alpha.id) return Promise.resolve(persisted);
      return normal(cmd, args, options);
    });
    useUIStore.setState({ probeTab: 'scratchpad' });
    render(<ScratchpadTab />);
    await act(async () => {});
    fireEvent.change(screen.getByLabelText('Scratch pad'), { target: { value: 'latest Alpha notes' } });
    await act(async () => { useMeshStore.getState().selectMesh(beta.id); });
    await act(async () => { useMeshStore.getState().selectMesh(alpha.id); });
    expect((screen.getByLabelText('Scratch pad') as HTMLTextAreaElement).disabled).toBe(true);
    await act(async () => { write.resolve(); });
    expect((screen.getByLabelText('Scratch pad') as HTMLTextAreaElement).value).toBe('latest Alpha notes');
  });
});

describe('Notes persistence boundary', () => {
  it('serializes same-mesh writes and allows other meshes to save independently', async () => {
    const first = deferred<void>();
    vi.mocked(invoke).mockImplementation((cmd, args) => cmd === 'set_mesh_scratchpad' && (args as { content: string }).content === 'first' ? first.promise : Promise.resolve());
    const saveFirst = setMeshScratchpad(alpha.id, 'first');
    const saveLatest = setMeshScratchpad(alpha.id, 'latest');
    await setMeshScratchpad(beta.id, 'independent');
    expect(vi.mocked(invoke).mock.calls.map(([, args]) => (args as { content: string }).content)).toEqual(['first', 'independent']);
    first.resolve();
    await Promise.all([saveFirst, saveLatest]);
    expect(vi.mocked(invoke).mock.calls.map(([, args]) => (args as { content: string }).content)).toEqual(['first', 'independent', 'latest']);
    expect(await getMeshScratchpad(alpha.id)).toBe('latest');
  });

  it('does not let an old read rejection evict a newer successful write', async () => {
    const oldRead = deferred<string>();
    vi.mocked(invoke).mockImplementation(cmd => cmd === 'get_mesh_scratchpad' ? oldRead.promise : Promise.resolve());
    const read = getMeshScratchpad(alpha.id);
    const rejected = expect(read).rejects.toThrow('Old read failed');
    await setMeshScratchpad(alpha.id, 'latest');
    oldRead.reject(new Error('Old read failed'));
    await rejected;
    expect(await getMeshScratchpad(alpha.id)).toBe('latest');
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'get_mesh_scratchpad')).toHaveLength(1);
  });

  it('continues the ordered queue after failure without evicting the newer value', async () => {
    const first = deferred<void>();
    vi.mocked(invoke).mockImplementation((cmd, args) => cmd === 'set_mesh_scratchpad' && (args as { content: string }).content === 'first' ? first.promise : Promise.resolve());
    const saveFirst = setMeshScratchpad(alpha.id, 'first');
    const rejected = expect(saveFirst).rejects.toThrow('First save failed');
    const saveLatest = setMeshScratchpad(alpha.id, 'latest');
    first.reject(new Error('First save failed'));
    await rejected;
    await saveLatest;
    expect(await getMeshScratchpad(alpha.id)).toBe('latest');
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'get_mesh_scratchpad')).toHaveLength(0);
  });
});

describe('Project Settings load ownership', () => {
  it('quarantines Alpha values when Beta fails to load, then retries Beta', async () => {
    const normal = vi.mocked(invoke).getMockImplementation()!;
    let failBeta = true;
    vi.mocked(invoke).mockImplementation((cmd, args, options) => {
      if (cmd === 'get_mesh_properties' && (args as { meshId: number }).meshId === beta.id && failBeta) return Promise.reject(new Error('Beta is unavailable'));
      return normal(cmd, args, options);
    });
    useUIStore.setState({ probeTab: 'properties' });
    render(<ProjectSettingsTab />);
    expect((await screen.findByLabelText('Name') as HTMLInputElement).value).toBe('Alpha');
    await act(async () => { useMeshStore.getState().selectMesh(beta.id); });
    expect(screen.queryByLabelText('Name')).toBeNull();
    expect(screen.getByText(/Beta is unavailable/)).toBeTruthy();
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'update_mesh_column')).toHaveLength(0);
    failBeta = false;
    fireEvent.click(screen.getByRole('button', { name: /retry/i }));
    expect((await screen.findByLabelText('Name') as HTMLInputElement).value).toBe('Beta');
  });

  it('ignores the late Alpha response after Beta has loaded', async () => {
    const read = deferred<ReturnType<typeof config>>();
    const normal = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation((cmd, args, options) => cmd === 'get_mesh_properties' && (args as { meshId: number }).meshId === alpha.id ? read.promise : normal(cmd, args, options));
    useUIStore.setState({ probeTab: 'properties' });
    render(<ProjectSettingsTab />);
    await act(async () => { useMeshStore.getState().selectMesh(beta.id); });
    expect((await screen.findByLabelText('Name') as HTMLInputElement).value).toBe('Beta');
    await act(async () => { read.resolve(config('Alpha')); });
    expect((screen.getByLabelText('Name') as HTMLInputElement).value).toBe('Beta');
  });
});

describe('Repository cleanup ownership', () => {
  it('blocks selecting old rows during refresh, including when refresh fails', async () => {
    const refresh = deferred<ReturnType<typeof pruneInfo>>();
    const normal = vi.mocked(invoke).getMockImplementation()!;
    let reads = 0;
    vi.mocked(invoke).mockImplementation((cmd, args, options) => cmd === 'get_git_prune_info' && ++reads > 1 ? refresh.promise : normal(cmd, args, options));
    useUIStore.setState({ probeTab: 'worktrees' });
    render(<RepositoryTab />);
    await screen.findByText('alpha-done');
    fireEvent.click(screen.getByRole('button', { name: 'Refresh branches and worktrees' }));
    const checkbox = screen.getByRole('checkbox', { name: /alpha-done/ }) as HTMLInputElement;
    expect(checkbox.disabled).toBe(true);
    fireEvent.click(checkbox);
    await act(async () => { refresh.reject(new Error('Refresh unavailable')); });
    expect((screen.getByRole('button', { name: 'Delete Selected' }) as HTMLButtonElement).disabled).toBe(true);
    expect(checkbox.checked).toBe(false);
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'delete_branches' || cmd === 'delete_worktrees')).toHaveLength(0);
  });

  it('drops a manual Alpha refresh that settles after switching to Beta', async () => {
    const refresh = deferred<ReturnType<typeof pruneInfo>>();
    const normal = vi.mocked(invoke).getMockImplementation()!;
    let alphaReads = 0;
    vi.mocked(invoke).mockImplementation((cmd, args, options) => {
      if (cmd === 'get_git_prune_info') {
        if ((args as { meshId: number }).meshId === beta.id) return Promise.resolve(pruneInfo('/repos/beta', 'beta-done'));
        if (++alphaReads > 1) return refresh.promise;
      }
      return normal(cmd, args, options);
    });
    useUIStore.setState({ probeTab: 'worktrees' });
    render(<RepositoryTab />);
    await screen.findByText('alpha-done');
    fireEvent.click(screen.getByRole('button', { name: 'Refresh branches and worktrees' }));
    await act(async () => { useMeshStore.getState().selectMesh(beta.id); });
    await screen.findByText('beta-done');
    await act(async () => { refresh.resolve(pruneInfo('/repos/alpha', 'alpha-stale')); });
    expect(screen.queryByText('alpha-stale')).toBeNull();
    expect(screen.getByText('beta-done')).toBeTruthy();
  });

  it('removes old cleanup targets and confirmation while the next mesh loads or fails', async () => {
    const betaRead = deferred<ReturnType<typeof pruneInfo>>();
    const normal = vi.mocked(invoke).getMockImplementation()!;
    vi.mocked(invoke).mockImplementation((cmd, args, options) => cmd === 'get_git_prune_info' && (args as { meshId: number }).meshId === beta.id ? betaRead.promise : normal(cmd, args, options));
    useUIStore.setState({ probeTab: 'worktrees' });
    render(<RepositoryTab />);
    await screen.findByText('alpha-done');
    fireEvent.click(screen.getByText(/Select recommended/));
    fireEvent.click(screen.getByRole('button', { name: 'Delete Selected' }));
    expect(screen.getByRole('dialog')).toBeTruthy();
    await act(async () => { useMeshStore.getState().selectMesh(beta.id); });
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(screen.queryByText('alpha-done')).toBeNull();
    await act(async () => { betaRead.reject(new Error('Beta cleanup unavailable')); });
    expect(screen.queryByText('alpha-done')).toBeNull();
    expect(vi.mocked(invoke).mock.calls.filter(([cmd]) => cmd === 'delete_branches' || cmd === 'delete_worktrees')).toHaveLength(0);
  });
});
