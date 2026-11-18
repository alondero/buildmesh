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

    const rows = Array.from(agyGroup.querySelectorAll<HTMLElement>('li'));
    stubRowRects(rows);

    const firstHandle = within(agyGroup).getByRole('button', { name: 'Reorder AGY Fast under Antigravity' });
    firstHandle.focus();
    fireEvent.keyDown(firstHandle, { key: ' ', code: 'Space' });
    expect(firstHandle.getAttribute('aria-pressed')).toBe('true');

    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    fireEvent.keyDown(document, { key: 'ArrowDown', code: 'ArrowDown' });
    fireEvent.keyDown(document, { key: ' ', code: 'Space' });

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

  it('rolls back optimistic order and shows an alert when reorder fails', async () => {
    const configs = [
      makeConfig('agy-1', 'AGY Fast', 'agy'),
      makeConfig('agy-2', 'AGY Deep', 'agy'),
    ];
    let rejectReorder!: (err: Error) => void;
    const reorder = vi.fn(
      () => new Promise<void>((_resolve, reject) => { rejectReorder = reject; }),
    );
    render(
      <LaunchConfigurations
        api={{
          list: async () => configs,
          targets: async () => [agyTarget],
          save: vi.fn(),
          remove: vi.fn(),
          reorder,
        }}
      />,
    );

    const agyGroup = await screen.findByTestId('launch-config-group-agy');
    stubRowRects(Array.from(agyGroup.querySelectorAll<HTMLElement>('li')));

    const firstHandle = within(agyGroup).getByRole('button', { name: 'Reorder AGY Fast under Antigravity' });
    firstHandle.focus();
    fireEvent.keyDown(firstHandle, { key: ' ', code: 'Space' });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    fireEvent.keyDown(document, { key: 'ArrowDown', code: 'ArrowDown' });
    fireEvent.keyDown(document, { key: ' ', code: 'Space' });

    // Optimistic order appears immediately before the request settles.
    expect(Array.from(agyGroup.querySelectorAll('li p')).map((p) => p.textContent)).toEqual([
      'AGY Deep · User-owned',
      'AGY Fast · User-owned',
    ]);

    await act(async () => {
      rejectReorder(new Error('preferences locked'));
    });

    await waitFor(() => {
      expect(screen.getByRole('alert').textContent).toContain('preferences locked');
    });
    expect(Array.from(agyGroup.querySelectorAll('li p')).map((p) => p.textContent)).toEqual([
      'AGY Fast · User-owned',
      'AGY Deep · User-owned',
    ]);
  });
});

describe('SpawnConfigurationMenu inline drag-and-drop', () => {
  it('reorders configurations inside the submenu without letting sibling hover swap the submenu mid-drag', async () => {
    const configs = [
      makeConfig('agy-1', 'AGY Fast', 'agy'),
      makeConfig('agy-2', 'AGY Deep', 'agy'),
      makeConfig('agy-3', 'AGY Review', 'agy'),
    ];
    vi.mocked(api.listSpawnConfigurations).mockResolvedValue(configs);

    render(<GroupedProviderMenu providers={[agyOption, opencodeOption]} onSelect={vi.fn()} />);
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

    // While dragging, hovering a sibling harness must NOT switch the submenu.
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

  it('rolls back order and shows error in submenu when reorderSpawnConfigurations fails', async () => {
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

    handles[0].focus();
    fireEvent.keyDown(handles[0], { key: ' ', code: 'Space' });
    await act(async () => {
      await new Promise((r) => setTimeout(r, 0));
    });
    fireEvent.keyDown(document, { key: 'ArrowDown', code: 'ArrowDown' });
    fireEvent.keyDown(document, { key: ' ', code: 'Space' });

    await waitFor(() => {
      expect(within(panel).getByRole('alert').textContent).toContain('disk full');
    });
    const configButtons = within(panel)
      .getAllByRole('menuitem')
      .filter((el) => ['AGY Fast', 'AGY Deep'].includes(el.textContent ?? ''));
    expect(configButtons.map((b) => b.textContent)).toEqual(['AGY Fast', 'AGY Deep']);
  });
});
