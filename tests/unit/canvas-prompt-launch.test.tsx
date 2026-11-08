import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { CanvasSpawnMenu } from '../../src/components/AgentNodeView/CanvasSpawnMenu';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { useMeshStore } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import type { Mesh } from '../../src/types/generated/Mesh';

vi.mock('../../src/hooks/useProviderList', () => ({
  useProviderList: () => [{ id: 'claude', harness_id: 'claude', label: 'Claude' }],
  __resetSharedProviderListForTests: () => undefined,
}));
vi.mock('../../src/lib/tauri/preferences', () => ({ listSavedPrompts: vi.fn().mockResolvedValue([]) }));
vi.mock('../../src/lib/tauri/provider', () => ({ getDefaultProvider: vi.fn().mockResolvedValue('claude') }));
vi.mock('../../src/components/Sidebar/SpawnButtonCluster', () => ({
  __resetDefaultChangedListeningForTests: () => undefined,
  SpawnButtonCluster: ({ onSelectProvider, disabled }: { onSelectProvider: (id: string, alt: boolean, config: string, login: string) => void; disabled: boolean }) =>
    <button disabled={disabled} onClick={() => onSelectProvider('codex', true, 'launch/fast', 'work')}>Spawn configuration</button>,
}));

const MESH = { id: 7, name: 'Target project', path: '/project', use_worktree: true } as Mesh;
const NODE = { id: 42, mesh_id: 7, provider: 'codex', status: 'idle' } as AgentNode;

beforeEach(() => {
  useMeshStore.setState({ meshes: [MESH], meshesById: new Map([[MESH.id, MESH]]) });
  useUIStore.getState().openCanvasSpawnMenu(MESH.id, { promptLibrary: true });
});

describe('canvas prompt launch', () => {
  it('retries a failed launch on the created node with the edited prompt and no duplicate creation', async () => {
    const select = vi.fn().mockImplementation(async (...args: unknown[]) => {
      (args[8] as (node: AgentNode) => void)(NODE);
      throw new Error('CLI not ready');
    });
    const spawn = vi.fn().mockResolvedValue(undefined);
    useAgentNodeStore.setState({ selectProviderForMesh: select, spawnAgent: spawn });
    render(<CanvasSpawnMenu meshId={MESH.id} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Explore this project' }));
    fireEvent.change(screen.getByLabelText('Initial prompt'), { target: { value: 'One-off task' } });
    fireEvent.click(screen.getByRole('button', { name: 'Spawn configuration' }));
    await screen.findByText('CLI not ready');
    expect(select).toHaveBeenCalledWith(7, MESH.name, MESH.path, 'codex', false, 'One-off task', 'launch/fast', 'work', expect.any(Function));
    expect(useUIStore.getState().canvasSpawnMenuMeshId).toBe(7);
    fireEvent.change(screen.getByLabelText('Initial prompt'), { target: { value: 'Revised one-off task' } });
    fireEvent.click(screen.getByRole('button', { name: 'Retry launch' }));
    await waitFor(() => expect(spawn).toHaveBeenCalledWith(42, 'codex', { prefill: 'Revised one-off task' }));
    expect(select).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(useUIStore.getState().canvasSpawnMenuMeshId).toBeNull());
  });

  it('does not let a completed old request close a newly opened spawn dialog', async () => {
    let finish!: (node: AgentNode) => void;
    const select = vi.fn().mockImplementation(() => new Promise<AgentNode>(resolve => { finish = resolve; }));
    useAgentNodeStore.setState({ selectProviderForMesh: select });
    const view = render(<CanvasSpawnMenu meshId={MESH.id} />);
    fireEvent.click(await screen.findByRole('button', { name: 'Explore this project' }));
    fireEvent.click(screen.getByRole('button', { name: 'Spawn configuration' }));
    await waitFor(() => expect(select).toHaveBeenCalledTimes(1));
    act(() => useUIStore.getState().closeCanvasSpawnMenu());
    view.unmount();
    act(() => useUIStore.getState().openCanvasSpawnMenu(99));
    await act(async () => { finish(NODE); });
    expect(useUIStore.getState().canvasSpawnMenuMeshId).toBe(99);
  });
});
