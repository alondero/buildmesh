import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { beforeEach, describe, expect, it, vi } from 'vitest';
import { CircuitRunCapacity } from '../../src/components/Probe/CircuitRunCapacity';
import { listMeshes, updateMeshCircuitRunCapacity } from '../../src/lib/tauri';
import { useMeshStore } from '../../src/stores/meshStore';

vi.mock('../../src/lib/tauri', () => ({ listMeshes: vi.fn(), updateMeshCircuitRunCapacity: vi.fn() }));

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: Error) => void;
  const promise = new Promise<T>((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

describe('Circuit run capacity', () => {
  beforeEach(() => {
    vi.clearAllMocks();
    vi.mocked(listMeshes).mockResolvedValue([]);
    useMeshStore.setState({ error: null, loading: false });
  });

  it('saves the selected mesh and keeps controls pending until the new mesh snapshot arrives', async () => {
    const saved = deferred<void>();
    const refreshed = deferred<Awaited<ReturnType<typeof listMeshes>>>();
    vi.mocked(updateMeshCircuitRunCapacity).mockReturnValue(saved.promise);
    vi.mocked(listMeshes).mockReturnValue(refreshed.promise);
    const { rerender } = render(<CircuitRunCapacity meshId={7} capacity={2} />);
    const select = screen.getByRole('combobox') as HTMLSelectElement;
    fireEvent.change(select, { target: { value: '4' } });
    expect(updateMeshCircuitRunCapacity).toHaveBeenCalledWith(7, 4);
    expect(select.disabled).toBe(true);
    expect(listMeshes).not.toHaveBeenCalled();
    await act(async () => { saved.resolve(); await saved.promise; });
    expect(select.disabled).toBe(true);
    await act(async () => { refreshed.resolve([]); await refreshed.promise; });
    rerender(<CircuitRunCapacity meshId={7} capacity={4} />);
    expect(select.disabled).toBe(false);
    expect(select.value).toBe('4');
  });

  it.each(['save', 'refresh'])('shows a %s failure and releases the controls for retry', async (phase) => {
    vi.mocked(updateMeshCircuitRunCapacity).mockResolvedValue(undefined);
    if (phase === 'save') vi.mocked(updateMeshCircuitRunCapacity).mockRejectedValue(new Error('save failed'));
    else vi.mocked(listMeshes).mockRejectedValue(new Error('refresh failed'));
    render(<CircuitRunCapacity meshId={7} capacity={2} />);
    fireEvent.change(screen.getByRole('combobox'), { target: { value: '3' } });
    expect((await screen.findByRole('alert')).textContent).toContain(`${phase} failed`);
    await waitFor(() => expect((screen.getByRole('combobox') as HTMLSelectElement).disabled).toBe(false));
    expect((screen.getByRole('combobox') as HTMLSelectElement).value).toBe('2');
    if (phase === 'save') expect(listMeshes).not.toHaveBeenCalled();
  });

  it.each(['old-first', 'new-first'])('keeps a switched mesh independent when saves finish %s', async (order) => {
    const oldSave = deferred();
    const newSave = deferred();
    vi.mocked(updateMeshCircuitRunCapacity).mockImplementation((mesh) => mesh === 7 ? oldSave.promise : newSave.promise);
    const { rerender, unmount } = render(<CircuitRunCapacity key={7} meshId={7} capacity={2} />);
    fireEvent.change(screen.getByRole('combobox'), { target: { value: '3' } });
    rerender(<CircuitRunCapacity key={8} meshId={8} capacity={5} />);
    fireEvent.change(screen.getByRole('combobox'), { target: { value: '6' } });
    const old = async () => { oldSave.reject(new Error('old mesh failed')); await oldSave.promise.catch(() => {}); };
    const newer = async () => { newSave.resolve(); await newSave.promise; };
    if (order === 'old-first') {
      await act(old);
      expect((screen.getByRole('combobox') as HTMLSelectElement).disabled).toBe(true);
      await act(newer);
    } else {
      await act(newer);
      await act(old);
    }
    expect(screen.queryByRole('alert')).toBeNull();
    expect((screen.getByRole('combobox') as HTMLSelectElement).disabled).toBe(false);
    expect(updateMeshCircuitRunCapacity).toHaveBeenCalledWith(8, 6);
    unmount();
  });
});
