import { beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen, waitFor } from '@testing-library/react';
import { invoke } from '@tauri-apps/api/core';
import { buildDefaultFixtures } from '../../scripts/ui-mock/default-fixtures';
import { MeshRecolorModal } from '../../src/components/Mesh/MeshRecolorModal';
import { useMeshStore } from '../../src/stores/meshStore';

const mockInvoke = vi.mocked(invoke);
const mesh = buildDefaultFixtures().list_meshes[0];

describe('MeshRecolorModal save recovery', () => {
  beforeEach(() => {
    useMeshStore.setState({ meshes: [mesh], meshesById: new Map([[mesh.id, mesh]]), error: null });
  });

  it('retains the chosen colour after a rejected save and closes only after a successful retry', async () => {
    mockInvoke.mockRejectedValueOnce(new Error('Colour could not be saved'));
    const onClose = vi.fn();
    render(<MeshRecolorModal meshId={mesh.id} meshName={mesh.name} currentColor={mesh.color!} onClose={onClose} />);
    const choice = screen.getAllByRole('button', { name: /Colour #/ })[0];
    fireEvent.click(choice);
    fireEvent.click(screen.getByRole('button', { name: 'Save', exact: true }));
    expect((await screen.findByRole('alert')).textContent).toContain('Colour could not be saved');
    expect(onClose).not.toHaveBeenCalled();
    expect(choice.getAttribute('aria-pressed')).toBe('true');
    expect(useMeshStore.getState().meshesById.get(mesh.id)?.color).toBe(mesh.color);
    mockInvoke.mockResolvedValueOnce(undefined);
    fireEvent.click(screen.getByRole('button', { name: 'Save', exact: true }));
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
    expect(useMeshStore.getState().meshesById.get(mesh.id)?.color).toBe(choice.title);
  });

  it('keeps the pending save and colour draft protected from dismissal', async () => {
    let resolve!: () => void;
    mockInvoke.mockReturnValueOnce(new Promise<void>(done => { resolve = done; }));
    const onClose = vi.fn();
    render(<MeshRecolorModal meshId={mesh.id} meshName={mesh.name} currentColor={mesh.color!} onClose={onClose} />);
    fireEvent.click(screen.getByRole('button', { name: 'Save', exact: true }));
    expect((screen.getByRole('button', { name: 'Cancel' }) as HTMLButtonElement).disabled).toBe(true);
    expect(screen.getAllByRole('button', { name: /Colour #/ })[0].matches(':disabled')).toBe(true);
    fireEvent.keyDown(window, { key: 'Escape' });
    expect(onClose).not.toHaveBeenCalled();
    resolve();
    await waitFor(() => expect(onClose).toHaveBeenCalledOnce());
  });
});
