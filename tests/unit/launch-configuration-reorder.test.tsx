import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { LaunchConfigurations } from '../../src/components/Providers/LaunchConfigurations';
import { GroupedProviderMenu } from '../../src/components/Providers/GroupedProviderMenu';
import {
  applyHarnessConfigurationOrder,
  groupConfigurationsByHarness,
  reorderConfigurationIds,
} from '../../src/lib/launchConfigurationOrder';
import type { SpawnOption } from '../../src/lib/groups';
import type { LaunchTarget } from '../../src/types/generated/LaunchTarget';
import type { SpawnConfiguration } from '../../src/types/generated/SpawnConfiguration';
import * as api from '../../src/lib/tauri/provider';

vi.mock('../../src/lib/tauri/provider', () => ({
  listProviders: vi.fn(),
  listSpawnConfigurations: vi.fn(),
  verifyLaunchConfiguration: vi.fn(),
  getLaunchTargets: vi.fn(),
  saveSpawnConfiguration: vi.fn(),
  deleteSpawnConfiguration: vi.fn(),
  reorderSpawnConfigurations: vi.fn(),
}));

const agyTarget: LaunchTarget = {
  id: 'agy',
  harness_id: 'agy',
  harness_name: 'Antigravity',
  provider_name: 'Native authentication',
  models: [],
  efforts: ['low', 'high'],
  route_attached: false,
  manual_model: true,
  supports_model: true,
  supports_extra_args: true,
};

const opencodeTarget: LaunchTarget = {
  id: 'opencode',
  harness_id: 'opencode',
  harness_name: 'OpenCode',
  provider_name: 'Native authentication',
  models: [],
  efforts: [],
  route_attached: false,
  manual_model: true,
  supports_model: true,
  supports_extra_args: true,
};

const makeConfig = (id: string, name: string, harnessId: string): SpawnConfiguration => ({
  id,
  name,
  spawn_option_id: harnessId,
  harness_id: harnessId,
  model: null,
  effort: null,
  extra_args: null,
});

const agyOption: SpawnOption = {
  id: 'agy',
  label: 'Antigravity',
  harness_id: 'agy',
  provider_id: null,
  is_proxied: false,
  group_key: 'agy',
  color: '',
  icon: '',
  capabilities: {
    harness_id: 'agy',
    supports_resume: true,
    auto_resume_on_startup: true,
    requires_attention_hook: false,
    produces_readable_transcript: true,
    supports_passive_turn_watcher: false,
    attention_capability: { kind: 'none' },
    supports_model_override: true,
    supports_effort_override: true,
    supports_extra_args: true,
    supports_agent: false,
    supports_prefill: true,
    is_plain_terminal: false,
    effort_control: { kind: 'none' },
    available_on: ['windows'],
  },
};

const opencodeOption: SpawnOption = {
  ...agyOption,
  id: 'opencode',
  label: 'OpenCode',
  harness_id: 'opencode',
  group_key: 'opencode',
  capabilities: { ...agyOption.capabilities!, harness_id: 'opencode' },
};

function deferred<T>() {
  let resolve!: (value: T) => void;
  let reject!: (error: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function stubRowRects(rows: HTMLElement[]) {
  rows.forEach((row, index) => {
    const top = index * 48;
    row.getBoundingClientRect = function () {
      return {
        width: 240,
        height: 48,
        top,
        left: 0,
        right: 240,
        bottom: top + 48,
        x: 0,
        y: top,
        toJSON() { return {}; },
      } as DOMRect;
    };
  });
}

async function keyboardDragDown(handle: HTMLElement, steps = 1) {
  handle.focus();
  fireEvent.keyDown(handle, { key: ' ', code: 'Space' });
  await act(async () => {
    await new Promise((r) => setTimeout(r, 0));
  });
  for (let i = 0; i < steps; i++) {
    fireEvent.keyDown(document, { key: 'ArrowDown', code: 'ArrowDown' });
  }
  fireEvent.keyDown(document, { key: ' ', code: 'Space' });
}

afterEach(cleanup);

beforeEach(() => {
  vi.resetAllMocks();
  vi.mocked(api.listProviders).mockResolvedValue([agyOption, opencodeOption]);
  vi.mocked(api.getLaunchTargets).mockResolvedValue([agyTarget, opencodeTarget]);
  vi.mocked(api.reorderSpawnConfigurations).mockResolvedValue(undefined);
});

describe('launchConfigurationOrder helpers', () => {
  it('reorders ids and preserves other harnesses and unmentioned slots', () => {
    expect(reorderConfigurationIds(['a', 'b', 'c'], 'a', 'c')).toEqual(['b', 'c', 'a']);
    expect(reorderConfigurationIds(['a', 'b', 'c'], 'b', 'b')).toEqual(['a', 'b', 'c']);
    expect(reorderConfigurationIds(['a', 'b'], 'a', 'missing')).toEqual(['a', 'b']);

    const initial = [
      makeConfig('agy-1', 'AGY 1', 'agy'),
      makeConfig('oc-1', 'OC 1', 'opencode'),
      makeConfig('agy-2', 'AGY 2', 'agy'),
      makeConfig('agy-3', 'AGY 3', 'agy'),
    ];
    const reordered = applyHarnessConfigurationOrder(initial, 'agy', ['agy-3', 'agy-1']);
    expect(reordered.map((c) => c.id)).toEqual(['agy-3', 'oc-1', 'agy-2', 'agy-1']);
  });

  it('groups configurations by harness using target display names and fallback order', () => {
    const groups = groupConfigurationsByHarness(
      [
        makeConfig('oc-1', 'OC 1', 'opencode'),
        makeConfig('agy-1', 'AGY 1', 'agy'),
        makeConfig('custom-1', 'Custom 1', 'custom-harness'),
        makeConfig('agy-2', 'AGY 2', 'agy'),
      ],
      [agyTarget, opencodeTarget],
    );
    expect(groups.map((g) => ({ id: g.harnessId, name: g.harnessName, ids: g.configurations.map((c) => c.id) }))).toEqual([
      { id: 'agy', name: 'Antigravity', ids: ['agy-1', 'agy-2'] },
      { id: 'opencode', name: 'OpenCode', ids: ['oc-1'] },
      { id: 'custom-harness', name: 'custom-harness', ids: ['custom-1'] },
    ]);
  });
});

describe('LaunchConfigurations grouped drag-and-drop', () => {
  it('groups by harness, shows reorder handles only for 2+ items, and reorders optimistically via keyboard drag', async () => {
    const configs = [
      makeConfig('agy-1', 'AGY Fast', 'agy'),
      makeConfig('agy-2', 'AGY Deep', 'agy'),
      makeConfig('agy-3', 'AGY Review', 'agy'),
      makeConfig('oc-1', 'OC Solo', 'opencode'),
    ];
    const reorder = vi.fn().mockResolvedValue(undefined);
    const onChanged = vi.fn();
    render(
      <LaunchConfigurations
        api={{
          list: async () => configs,
          targets: async () => [agyTarget, opencodeTarget],
          save: vi.fn(),
          remove: vi.fn(),
          reorder,
        }}
        onChanged={onChanged}
      />,
    );

    const agyGroup = await screen.findByTestId('launch-config-group-agy');
    const ocGroup = screen.getByTestId('launch-config-group-opencode');

    expect(within(agyGroup).getAllByRole('button', { name: /^Reorder / })).toHaveLength(3);
    expect(within(ocGroup).queryByRole('button', { name: /^Reorder / })).toBeNull();

    stubRowRects(Array.from(agyGroup.querySelectorAll<HTMLElement>('li')));

    const firstHandle = within(agyGroup).getByRole('button', { name: 'Reorder AGY Fast under Antigravity' });
    await keyboardDragDown(firstHandle);

    await waitFor(() => {
      expect(reorder).toHaveBeenCalledWith('agy', ['agy-2', 'agy-1', 'agy-3']);
    });
    expect(onChanged).toHaveBeenCalledTimes(1);

    const renderedNames = Array.from(agyGroup.querySelectorAll('li p')).map((p) => p.textContent);
    expect(renderedNames).toEqual([
      'AGY Deep · User-owned',
      'AGY Fast · User-owned',
      'AGY Review · User-owned',
    ]);
  });

  it('serializes overlapping drags within the same harness so an older write can never overwrite a newer order', async () => {
    let backendConfigs = [
      makeConfig('agy-1', 'AGY Fast', 'agy'),
      makeConfig('agy-2', 'AGY Deep', 'agy'),
      makeConfig('agy-3', 'AGY Review', 'agy'),
    ];
    const firstWrite = deferred<void>();
    const secondWrite = deferred<void>();
    const reorderCalls: Array<{ harnessId: string; ids: string[] }> = [];
    const reorder = vi.fn((harnessId: string, ids: string[]) => {
      reorderCalls.push({ harnessId, ids });
      const gate = reorderCalls.length === 1 ? firstWrite : secondWrite;
      return gate.promise.then(() => {
        backendConfigs = applyHarnessConfigurationOrder(backendConfigs, harnessId, ids);
      });
    });

    render(
      <LaunchConfigurations
        api={{
          list: async () => backendConfigs,
          targets: async () => [agyTarget],
          save: vi.fn(),
          remove: vi.fn(),
          reorder,
        }}
      />,
    );

    const agyGroup = await screen.findByTestId('launch-config-group-agy');
    stubRowRects(Array.from(agyGroup.querySelectorAll<HTMLElement>('li')));

    // Drag 1: move AGY Fast down 1 -> [AGY Deep, AGY Fast, AGY Review]
    await keyboardDragDown(within(agyGroup).getByRole('button', { name: 'Reorder AGY Fast under Antigravity' }));
    expect(reorderCalls).toEqual([{ harnessId: 'agy', ids: ['agy-2', 'agy-1', 'agy-3'] }]);

    // Drag 2 while Drag 1 is STILL in flight: move AGY Fast down 1 again -> [AGY Deep, AGY Review, AGY Fast]
    stubRowRects(Array.from(agyGroup.querySelectorAll<HTMLElement>('li')));
    await keyboardDragDown(within(agyGroup).getByRole('button', { name: 'Reorder AGY Fast under Antigravity' }));

    // Drag 2 MUST NOT fire concurrently while Drag 1 is still in flight.
    expect(reorderCalls).toHaveLength(1);
    expect(Array.from(agyGroup.querySelectorAll('li p')).map((p) => p.textContent)).toEqual([
      'AGY Deep · User-owned',
      'AGY Review · User-owned',
      'AGY Fast · User-owned',
    ]);

    // Completing Drag 1 immediately dispatches Drag 2 strictly afterwards.
    await act(async () => { firstWrite.resolve(undefined); });
    await waitFor(() => {
      expect(reorderCalls).toEqual([
        { harnessId: 'agy', ids: ['agy-2', 'agy-1', 'agy-3'] },
        { harnessId: 'agy', ids: ['agy-2', 'agy-3', 'agy-1'] },
      ]);
    });

    await act(async () => { secondWrite.resolve(undefined); });
    await waitFor(() => {
      expect(backendConfigs.map((c) => c.id)).toEqual(['agy-2', 'agy-3', 'agy-1']);
    });
  });

  it('rolls back only the failed harness when overlapping drags span different harnesses', async () => {
    let backendConfigs = [
      makeConfig('agy-1', 'AGY Fast', 'agy'),
      makeConfig('agy-2', 'AGY Deep', 'agy'),
      makeConfig('oc-1', 'OC Alpha', 'opencode'),
      makeConfig('oc-2', 'OC Beta', 'opencode'),
    ];
    const agyWrite = deferred<void>();
    const ocWrite = deferred<void>();
    const list = vi.fn(async () => backendConfigs);
    const reorder = vi.fn((harnessId: string, ids: string[]) => {
      if (harnessId === 'agy') return agyWrite.promise;
      return ocWrite.promise.then(() => {
        backendConfigs = applyHarnessConfigurationOrder(backendConfigs, harnessId, ids);
      });
    });

    render(
      <LaunchConfigurations
        api={{
          list,
          targets: async () => [agyTarget, opencodeTarget],
          save: vi.fn(),
          remove: vi.fn(),
          reorder,
        }}
      />,
    );

    const agyGroup = await screen.findByTestId('launch-config-group-agy');
    const ocGroup = screen.getByTestId('launch-config-group-opencode');
    stubRowRects(Array.from(agyGroup.querySelectorAll<HTMLElement>('li')));
    stubRowRects(Array.from(ocGroup.querySelectorAll<HTMLElement>('li')));

    // Start drag on Antigravity (pending), then drag on OpenCode (queued).
    await keyboardDragDown(within(agyGroup).getByRole('button', { name: 'Reorder AGY Fast under Antigravity' }));
    await keyboardDragDown(within(ocGroup).getByRole('button', { name: 'Reorder OC Alpha under OpenCode' }));

    // Antigravity fails -> ONLY Antigravity rolls back; OpenCode proceeds and succeeds!
    await act(async () => { agyWrite.reject(new Error('agy write refused')); });
    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toContain('agy write refused');
    });
    expect(Array.from(agyGroup.querySelectorAll('li p')).map((p) => p.textContent)).toEqual([
      'AGY Fast · User-owned',
      'AGY Deep · User-owned',
    ]);
    expect(Array.from(ocGroup.querySelectorAll('li p')).map((p) => p.textContent)).toEqual([
      'OC Beta · User-owned',
      'OC Alpha · User-owned',
    ]);

    await act(async () => { ocWrite.resolve(undefined); });

    // Trailing authoritative refresh converges with backend while keeping the failure alert visible.
    await waitFor(() => {
      expect(list.mock.calls.length).toBeGreaterThanOrEqual(2);
    });
    expect(screen.getByRole('alert').textContent).toContain('agy write refused');
    expect(Array.from(agyGroup.querySelectorAll('li p')).map((p) => p.textContent)).toEqual([
      'AGY Fast · User-owned',
      'AGY Deep · User-owned',
    ]);
    expect(Array.from(ocGroup.querySelectorAll('li p')).map((p) => p.textContent)).toEqual([
      'OC Beta · User-owned',
      'OC Alpha · User-owned',
    ]);
  });

  it('drops an in-flight stale list refresh during a pending reorder and converges with a trailing read', async () => {
    const initial = [
      makeConfig('agy-1', 'AGY Fast', 'agy'),
      makeConfig('agy-2', 'AGY Deep', 'agy'),
    ];
    const reordered = [initial[1], initial[0]];
    const staleRefresh = deferred<SpawnConfiguration[]>();
    const writeGate = deferred<void>();
    let listCalls = 0;
    const list = vi.fn(() => {
      listCalls += 1;
      if (listCalls === 1) return Promise.resolve(initial);
      if (listCalls === 2) return staleRefresh.promise;
      return Promise.resolve(reordered);
    });
    const reorder = vi.fn(() => writeGate.promise);
    const mockApi = {
      list,
      targets: async () => [agyTarget],
      save: vi.fn(),
      remove: vi.fn(),
      reorder,
    };

    const { rerender } = render(<LaunchConfigurations api={mockApi} refreshToken={1} />);
    const agyGroup = await screen.findByTestId('launch-config-group-agy');
    expect(listCalls).toBe(1);

    // Trigger an in-flight refresh (e.g. from a background provider-list update) before the drag starts.
    rerender(<LaunchConfigurations api={mockApi} refreshToken={2} />);
    await waitFor(() => expect(listCalls).toBe(2));

    // User drags while the older refresh is still in flight.
    stubRowRects(Array.from(agyGroup.querySelectorAll<HTMLElement>('li')));
    await keyboardDragDown(within(agyGroup).getByRole('button', { name: 'Reorder AGY Fast under Antigravity' }));
    expect(Array.from(agyGroup.querySelectorAll('li p')).map((p) => p.textContent)).toEqual([
      'AGY Deep · User-owned',
      'AGY Fast · User-owned',
    ]);

    // The stale refresh resolves with the OLD order — it MUST NOT overwrite the optimistic reorder.
    await act(async () => { staleRefresh.resolve(initial); });
    expect(Array.from(agyGroup.querySelectorAll('li p')).map((p) => p.textContent)).toEqual([
      'AGY Deep · User-owned',
      'AGY Fast · User-owned',
    ]);

    // Once the reorder settles, a trailing authoritative refresh runs automatically (even without a new refreshToken).
    await act(async () => { writeGate.resolve(undefined); });
    await waitFor(() => expect(listCalls).toBe(3));
    expect(Array.from(agyGroup.querySelectorAll('li p')).map((p) => p.textContent)).toEqual([
      'AGY Deep · User-owned',
      'AGY Fast · User-owned',
    ]);
  });

  it('rolls back to the last confirmed order when a second same-harness drag fails after the first succeeded', async () => {
    let backendConfigs = [
      makeConfig('agy-1', 'AGY Fast', 'agy'),
      makeConfig('agy-2', 'AGY Deep', 'agy'),
      makeConfig('agy-3', 'AGY Review', 'agy'),
    ];
    const firstWrite = deferred<void>();
    const secondWrite = deferred<void>();
    let callCount = 0;
    const reorder = vi.fn((harnessId: string, ids: string[]) => {
      callCount += 1;
      if (callCount === 1) {
        return firstWrite.promise.then(() => {
          backendConfigs = applyHarnessConfigurationOrder(backendConfigs, harnessId, ids);
        });
      }
      return secondWrite.promise;
    });
    render(
      <LaunchConfigurations
        api={{
          list: async () => backendConfigs,
          targets: async () => [agyTarget],
          save: vi.fn(),
          remove: vi.fn(),
          reorder,
        }}
      />,
    );

    const agyGroup = await screen.findByTestId('launch-config-group-agy');
    stubRowRects(Array.from(agyGroup.querySelectorAll<HTMLElement>('li')));
    await keyboardDragDown(within(agyGroup).getByRole('button', { name: 'Reorder AGY Fast under Antigravity' }));

    stubRowRects(Array.from(agyGroup.querySelectorAll<HTMLElement>('li')));
    await keyboardDragDown(within(agyGroup).getByRole('button', { name: 'Reorder AGY Fast under Antigravity' }));

    await act(async () => { firstWrite.resolve(undefined); });
    await waitFor(() => expect(callCount).toBe(2));

    await act(async () => { secondWrite.reject(new Error('preferences locked')); });
    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toContain('preferences locked');
    });
    // Must roll back to drag 1's confirmed order ([AGY Deep, AGY Fast, AGY Review]), not the initial order.
    expect(Array.from(agyGroup.querySelectorAll('li p')).map((p) => p.textContent)).toEqual([
      'AGY Deep · User-owned',
      'AGY Fast · User-owned',
      'AGY Review · User-owned',
    ]);
  });
});

describe('SpawnConfigurationMenu inline drag-and-drop', () => {
  it('reorders configurations inside the submenu without letting sibling or leadingAction hover close the submenu mid-drag', async () => {
    const configs = [
      makeConfig('agy-1', 'AGY Fast', 'agy'),
      makeConfig('agy-2', 'AGY Deep', 'agy'),
      makeConfig('agy-3', 'AGY Review', 'agy'),
    ];
    vi.mocked(api.listSpawnConfigurations).mockResolvedValue(configs);

    render(
      <GroupedProviderMenu
        providers={[agyOption, opencodeOption]}
        leadingAction={{ id: '__inherit', label: 'App default', onSelect: vi.fn() }}
        onSelect={vi.fn()}
      />,
    );
    fireEvent.click(screen.getByRole('button', { name: 'Antigravity configurations', exact: true }));
    const panel = await screen.findByTestId('spawn-configurations');

    // Roving tabindex invariant still holds with reorder handles included.
    const menuItems = within(panel).getAllByRole('menuitem');
    expect(menuItems.filter((item) => item.tabIndex === 0)).toHaveLength(1);

    // ArrowDown from "Spawn with defaults" lands on the first configuration to launch, not its drag handle.
    fireEvent.keyDown(menuItems[0], { key: 'ArrowDown' });
    expect(document.activeElement?.textContent).toContain('AGY Fast');

    const handles = within(panel).getAllByRole('menuitem', { name: /^Reorder / });
    expect(handles).toHaveLength(3);
    stubRowRects(handles.map((h) => h.parentElement as HTMLElement));

    const firstHandle = handles[0];
    firstHandle.focus();
    fireEvent.keyDown(firstHandle, { key: ' ', code: 'Space' });
    expect(firstHandle.getAttribute('aria-pressed')).toBe('true');

    // While dragging, hovering either a sibling harness OR the leading action must NOT close/swap the submenu.
    fireEvent.mouseEnter(screen.getByRole('menuitem', { name: 'App default', exact: true }));
    fireEvent.mouseEnter(screen.getByRole('menuitem', { name: 'OpenCode', exact: true }));
    expect(within(panel).getByRole('menu', { name: 'Antigravity configurations' })).toBeTruthy();

    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    fireEvent.keyDown(document, { key: 'ArrowDown', code: 'ArrowDown' });
    fireEvent.keyDown(document, { key: ' ', code: 'Space' });

    await waitFor(() => {
      expect(api.reorderSpawnConfigurations).toHaveBeenCalledWith('agy', ['agy-2', 'agy-1', 'agy-3']);
    });

    const configButtons = within(panel)
      .getAllByRole('menuitem')
      .filter((el) => ['AGY Fast', 'AGY Deep', 'AGY Review'].includes(el.textContent ?? ''));
    expect(configButtons.map((b) => b.textContent)).toEqual(['AGY Deep', 'AGY Fast', 'AGY Review']);
  });

  it('serializes overlapping drags in the submenu so older requests never overtake newer ones', async () => {
    const configs = [
      makeConfig('agy-1', 'AGY Fast', 'agy'),
      makeConfig('agy-2', 'AGY Deep', 'agy'),
      makeConfig('agy-3', 'AGY Review', 'agy'),
    ];
    vi.mocked(api.listSpawnConfigurations).mockResolvedValue(configs);
    const firstWrite = deferred<void>();
    const secondWrite = deferred<void>();
    const calls: string[][] = [];
    vi.mocked(api.reorderSpawnConfigurations).mockImplementation((_harnessId, ids) => {
      calls.push(ids);
      return calls.length === 1 ? firstWrite.promise : secondWrite.promise;
    });

    render(<GroupedProviderMenu providers={[agyOption]} onSelect={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Antigravity configurations', exact: true }));
    const panel = await screen.findByTestId('spawn-configurations');

    const getHandles = () => within(panel).getAllByRole('menuitem', { name: /^Reorder / });
    stubRowRects(getHandles().map((h) => h.parentElement as HTMLElement));
    await keyboardDragDown(within(panel).getByRole('menuitem', { name: 'Reorder AGY Fast' }));
    expect(calls).toEqual([['agy-2', 'agy-1', 'agy-3']]);

    stubRowRects(getHandles().map((h) => h.parentElement as HTMLElement));
    await keyboardDragDown(within(panel).getByRole('menuitem', { name: 'Reorder AGY Fast' }));
    // Second drag waits until first resolves.
    expect(calls).toHaveLength(1);

    await act(async () => { firstWrite.resolve(undefined); });
    await waitFor(() => {
      expect(calls).toEqual([
        ['agy-2', 'agy-1', 'agy-3'],
        ['agy-2', 'agy-3', 'agy-1'],
      ]);
    });
    await act(async () => { secondWrite.resolve(undefined); });
  });

  it('cancels an active keyboard drag on Escape without closing the submenu', async () => {
    const configs = [
      makeConfig('agy-1', 'AGY Fast', 'agy'),
      makeConfig('agy-2', 'AGY Deep', 'agy'),
    ];
    vi.mocked(api.listSpawnConfigurations).mockResolvedValue(configs);

    render(<GroupedProviderMenu providers={[agyOption]} onSelect={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Antigravity configurations', exact: true }));
    const panel = await screen.findByTestId('spawn-configurations');

    const firstHandle = within(panel).getByRole('menuitem', { name: 'Reorder AGY Fast' });
    firstHandle.focus();
    fireEvent.keyDown(firstHandle, { key: ' ', code: 'Space' });
    expect(firstHandle.getAttribute('aria-pressed')).toBe('true');

    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    fireEvent.keyDown(document, { key: 'Escape', code: 'Escape' });

    expect(api.reorderSpawnConfigurations).not.toHaveBeenCalled();
    expect(screen.getByTestId('spawn-configurations')).toBeTruthy();
    expect(firstHandle.getAttribute('aria-pressed')).not.toBe('true');
  });

  it('rolls back order, shows error, and refreshes authoritatively in submenu when reorderSpawnConfigurations fails', async () => {
    const configs = [
      makeConfig('agy-1', 'AGY Fast', 'agy'),
      makeConfig('agy-2', 'AGY Deep', 'agy'),
    ];
    vi.mocked(api.listSpawnConfigurations).mockResolvedValue(configs);
    vi.mocked(api.reorderSpawnConfigurations).mockRejectedValueOnce(new Error('disk full'));

    render(<GroupedProviderMenu providers={[agyOption]} onSelect={vi.fn()} />);
    fireEvent.click(screen.getByRole('button', { name: 'Antigravity configurations', exact: true }));
    const panel = await screen.findByTestId('spawn-configurations');

    const handles = within(panel).getAllByRole('menuitem', { name: /^Reorder / });
    stubRowRects(handles.map((h) => h.parentElement as HTMLElement));

    await keyboardDragDown(handles[0]);

    await waitFor(() => {
      expect(within(panel).getByRole('alert').textContent).toContain('disk full');
    });
    await waitFor(() => {
      expect(api.listSpawnConfigurations).toHaveBeenCalledTimes(2);
    });
    const configButtons = within(panel)
      .getAllByRole('menuitem')
      .filter((el) => ['AGY Fast', 'AGY Deep'].includes(el.textContent ?? ''));
    expect(configButtons.map((b) => b.textContent)).toEqual(['AGY Fast', 'AGY Deep']);
  });
});
