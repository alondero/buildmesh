import { beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { ProbePanel } from '../../src/components/Probe/ProbePanel';
import { useMeshStore } from '../../src/stores/meshStore';
import { useAgentNodeStore } from '../../src/stores/agentNodeStore';
import { useUIStore } from '../../src/stores/uiStore';
import type { Mesh } from '../../src/types/generated/Mesh';
import type { AgentNode } from '../../src/types/generated/AgentNode';
import { listSavedPrompts } from '../../src/lib/tauri/preferences';
import { getDefaultProvider } from '../../src/lib/tauri/provider';

vi.mock('../../src/lib/tauri/preferences', () => ({ listSavedPrompts: vi.fn().mockResolvedValue([]) }));
vi.mock('../../src/lib/tauri/provider', () => ({ getDefaultProvider: vi.fn().mockResolvedValue('claude') }));
vi.mock('../../src/hooks/useProviderList', () => ({ useProviderList: () => [], __resetSharedProviderListForTests: () => undefined }));
const MESH = { id: 7, name: 'Selected Mesh', path: '/selected', base_ref: 'origin/develop', use_worktree: true } as Mesh;
const OTHER = { ...MESH, id: 8, name: 'Other Mesh', base_ref: 'origin/release' };
beforeEach(() => {
  vi.clearAllMocks();
  useMeshStore.setState({ selectedMeshId: MESH.id, meshes: [MESH, OTHER], meshesById: new Map([[7, MESH], [8, OTHER]]) });
  useAgentNodeStore.setState({ activeNodeId: 42, nodesById: { 42: { id: 42, mesh_id: 8, name: 'Other agent', path: '/agent-checkout' } as AgentNode } });
  useUIStore.setState({ viewMode: 'mesh' });
  useUIStore.getState().openProbeTab('prompts');
});

describe('Prompts Probe context', () => {
  it('keeps the shared library mounted across Mesh selection changes and ignores focused checkouts', async () => {
    render(<ProbePanel />);
    await screen.findByTestId('prompt-library');
    await waitFor(() => expect((screen.getByRole('button', { name: 'New prompt' }) as HTMLButtonElement).disabled).toBe(false));
    expect(screen.getByTestId('probe-context-subject').textContent).toContain('Selected Mesh');
    expect(screen.getByTestId('prompt-launch-context').textContent).toContain('origin/develop');
    fireEvent.click(screen.getByRole('button', { name: 'Explore this project' }));
    act(() => useMeshStore.getState().selectMesh(OTHER.id));
    expect(screen.getByTestId('prompt-launch-context').textContent).toContain('origin/release');
    expect(document.querySelector('[data-prompt-body-expanded]')).toBeTruthy();
    expect(listSavedPrompts).toHaveBeenCalledTimes(1);
  });
  it('allows browsing and authoring without a Mesh instead of showing the shell context guard', async () => {
    useMeshStore.setState({ selectedMeshId: null });
    useAgentNodeStore.setState({ activeNodeId: null });
    render(<ProbePanel />);
    await screen.findByTestId('prompt-library');
    expect(screen.queryByTestId('probe-context-empty')).toBeNull();
    expect(screen.getByRole('combobox', { name: 'Spawn in Mesh' })).toBeTruthy();
    await waitFor(() => expect((screen.getByRole('button', { name: 'New prompt' }) as HTMLButtonElement).disabled).toBe(false));
    expect(getDefaultProvider).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('button', { name: 'New prompt' }));
    expect(screen.getByRole('dialog')).toBeTruthy();
  });
});
