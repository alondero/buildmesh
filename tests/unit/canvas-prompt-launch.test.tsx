import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import { CanvasSpawnMenu } from '../../src/components/AgentNodeView/CanvasSpawnMenu';
import { useMeshStore } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';
import type { Mesh } from '../../src/types/generated/Mesh';

vi.mock('../../src/hooks/useProviderList', () => ({
  useProviderList: () => [{ id: 'claude', harness_id: 'claude', label: 'Claude' }],
  __resetSharedProviderListForTests: () => undefined,
}));
const MESH = { id: 7, name: 'Target project', path: '/project', use_worktree: true } as Mesh;
beforeEach(() => {
  useMeshStore.setState({ selectedMeshId: null, meshes: [MESH], meshesById: new Map([[MESH.id, MESH]]) });
  useUIStore.getState().openCanvasSpawnMenu(MESH.id);
});

describe('canvas prompt shortcut', () => {
  it('closes the spawn dialog and opens the Prompts Probe in the requested Mesh', () => {
    render(<CanvasSpawnMenu meshId={MESH.id} />);
    expect(screen.queryByTestId('prompt-library')).toBeNull();
    fireEvent.click(screen.getByRole('button', { name: 'Choose a prompt…' }));
    expect(useUIStore.getState().canvasSpawnMenuMeshId).toBeNull();
    expect(useUIStore.getState().probeTab).toBe('prompts');
    expect(useUIStore.getState().probeOpen).toBe(true);
    expect(useMeshStore.getState().selectedMeshId).toBe(MESH.id);
  });
});
