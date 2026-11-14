import { useState } from 'react';
import { describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { PromptSpawnButton } from '../../src/components/Prompts/PromptSpawnButton';
import type { Mesh } from '../../src/types/generated/Mesh';
import type { SpawnOption } from '../../src/lib/groups';

const MESH = { id: 7, name: 'First Mesh' } as Mesh;
const OTHER = { id: 8, name: 'Second Mesh' } as Mesh;
const MESHES = [MESH, OTHER];
const PROVIDERS = [{ id: 'claude', label: 'Anthropic', harness_id: 'claude', group_key: 'claude', provider_id: null, is_proxied: false }] as SpawnOption[];
const props = { meshId: null, meshes: MESHES, promptId: 'test', name: 'Test prompt', providers: PROVIDERS, busy: false, disabled: false };
const picker = () => screen.queryByRole('menu', { name: 'Choose a Mesh to spawn in' });
const trigger = () => screen.getByRole('button', { name: 'Spawn Test prompt', exact: true });

describe('prompt launch Mesh picker', () => {
  it('chooses a Mesh from the primary Spawn path and returns focus to its trigger', async () => {
    const onSpawn = vi.fn();
    render(<PromptSpawnButton {...props} onSpawn={onSpawn} />);
    fireEvent.click(trigger());
    expect(picker()).toBeTruthy();
    expect(onSpawn).not.toHaveBeenCalled();
    fireEvent.click(screen.getByRole('menuitem', { name: OTHER.name }));
    expect(onSpawn).toHaveBeenCalledWith(undefined, undefined, undefined, OTHER);
    expect(picker()).toBeNull();
    await waitFor(() => expect(document.activeElement).toBe(trigger()));
  });

  it('moves focus with menu keys and restores trigger focus on Escape', () => {
    render(<PromptSpawnButton {...props} onSpawn={vi.fn()} />);
    fireEvent.click(trigger());
    const first = screen.getByRole('menuitem', { name: MESH.name });
    const second = screen.getByRole('menuitem', { name: OTHER.name });
    expect(document.activeElement).toBe(first);
    fireEvent.keyDown(first, { key: 'ArrowDown' });
    expect(document.activeElement).toBe(second);
    fireEvent.keyDown(second, { key: 'Home' });
    expect(document.activeElement).toBe(first);
    fireEvent.keyDown(first, { key: 'End' });
    expect(document.activeElement).toBe(second);
    fireEvent.keyDown(second, { key: 'Escape' });
    expect(picker()).toBeNull();
    expect(document.activeElement).toBe(trigger());
  });

  it('closes when the user clicks outside', () => {
    const onSpawn = vi.fn();
    render(<PromptSpawnButton {...props} onSpawn={onSpawn} />);
    fireEvent.click(trigger());
    expect(picker()).toBeTruthy();
    fireEvent.mouseDown(document.body);
    expect(picker()).toBeNull();
    expect(onSpawn).not.toHaveBeenCalled();
  });

  it('closes on Tab without moving focus to the spawn trigger', () => {
    render(<PromptSpawnButton {...props} onSpawn={vi.fn()} />);
    fireEvent.click(trigger());
    const focus = vi.spyOn(trigger(), 'focus');
    fireEvent.keyDown(screen.getByRole('menuitem', { name: MESH.name }), { key: 'Tab' });
    expect(picker()).toBeNull();
    expect(focus).not.toHaveBeenCalled();
  });

  it.each(['disabled', 'busy'] as const)('discards a pending choice when %s becomes true', flag => {
    const onSpawn = vi.fn();
    const view = render(<PromptSpawnButton {...props} onSpawn={onSpawn} />);
    fireEvent.click(trigger());
    expect(picker()).toBeTruthy();
    view.rerender(<PromptSpawnButton {...props} {...{ [flag]: true }} onSpawn={onSpawn} />);
    expect(picker()).toBeNull();
    view.rerender(<PromptSpawnButton {...props} onSpawn={onSpawn} />);
    expect(picker()).toBeNull();
    expect(onSpawn).not.toHaveBeenCalled();
  });

  it('discards pending choices when the inline target changes', () => {
    const onSpawn = vi.fn();
    const view = render(<PromptSpawnButton {...props} onSpawn={onSpawn} />);
    fireEvent.click(trigger());
    expect(picker()).toBeTruthy();
    view.rerender(<PromptSpawnButton {...props} meshId={MESH.id} onSpawn={onSpawn} />);
    expect(picker()).toBeNull();
    view.rerender(<PromptSpawnButton {...props} onSpawn={onSpawn} />);
    expect(picker()).toBeNull();
    expect(onSpawn).not.toHaveBeenCalled();
  });

  it.each([false, true])('holds focus during an async launch and respects user navigation away: %s', async navigateAway => {
    let finish!: () => void;
    const pending = new Promise<void>(resolve => { finish = resolve; });
    function Launch() {
      const [meshId, setMeshId] = useState<number | null>(null);
      const [busy, setBusy] = useState(false);
      return <><PromptSpawnButton {...props} meshId={meshId} busy={busy} disabled={busy}
        onSpawn={async (_provider, _configuration, _login, mesh) => {
          setMeshId(mesh!.id); setBusy(true); await pending; setBusy(false);
        }} /><button type="button">Other action</button></>;
    }
    render(<Launch />);
    fireEvent.click(trigger());
    fireEvent.click(screen.getByRole('menuitem', { name: MESH.name }));
    expect(document.activeElement).not.toBe(document.body);
    expect((trigger() as HTMLButtonElement).disabled).toBe(true);
    const other = screen.getByRole('button', { name: 'Other action' });
    if (navigateAway) other.focus();
    await act(async () => finish());
    await waitFor(() => expect(document.activeElement).toBe(navigateAway ? other : trigger()));
  });
});
