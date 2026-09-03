/**
 * Issue #1502 — in-place node regeneration + Regenerate action on the Node toolbar.
 *
 * Pins:
 *  - shared `splitRegenerateTargets` / `hasRegenerateTargets` partition the
 *    full provider list into current (kick-start) + alternates;
 *  - the shared `RegenerateProviderMenu` pins `Current (<label>)` on top;
 *  - `GridNodeHeader` renders an inline Regenerate toolbar button at
 *    wide/xl/medium tiers whose picker includes the current provider;
 *  - at slim/compact the same picker collapses into the kebab overflow menu;
 *  - picking the current provider fires `regenerateAgentNode(nodeId,
 *    currentProviderId)` (idle) or opens the running-node confirm dialog.
 */
import { describe, it, expect, vi, beforeEach, afterEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { act } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useAgentNodeStore, type AgentNode } from '../../src/stores/agentNodeStore';
import { useMeshStore, type Mesh } from '../../src/stores/meshStore';
import { useUIStore } from '../../src/stores/uiStore';
import { __resetProviderCachesForTests } from '../../src/lib/tauri';
import { splitRegenerateTargets, hasRegenerateTargets } from '../../src/lib/regenerate';
import { RegenerateProviderMenu } from '../../src/components/Providers/RegenerateProviderMenu';
import type { ProviderInfo } from '../../src/types/generated/ProviderInfo';
import type { SpawnOption } from '../../src/lib/groups';
import { colorClassForProvider } from '../../src/lib/groups';
import { seedAgentNodes } from './helpers/seedAgentNodes';

// ----- Fake ResizeObserver (mirrors grid-node-header-responsive.test.tsx) -----
type ROCallback = (entries: Array<Pick<ResizeObserverEntry, 'contentRect'>>) => void;
const roCallbacks = new Map<Element, ROCallback>();
class FakeResizeObserver {
  private cb: ROCallback;
  private observed = new Set<Element>();
  constructor(cb: ROCallback) {
    this.cb = cb;
  }
  observe(el: Element) {
    roCallbacks.set(el, this.cb);
    this.observed.add(el);
  }
  unobserve(el: Element) {
    roCallbacks.delete(el);
    this.observed.delete(el);
  }
  disconnect() {
    for (const el of this.observed) roCallbacks.delete(el);
    this.observed.clear();
  }
}
function fireResize(el: Element, width: number) {
  const cb = roCallbacks.get(el);
  if (!cb) throw new Error('No ResizeObserver registered for element');
  act(() => {
    cb([{ contentRect: { width, height: 0, top: 0, left: 0, right: width, bottom: 0, x: 0, y: 0 } } as unknown as ResizeObserverEntry]);
  });
}

vi.mock('../../src/components/BuildRun/BuildRunDropdown', () => ({
  BuildRunDropdown: () => <span data-testid="build-run">Build</span>,
}));
vi.mock('../../src/hooks/useGitSummary', () => ({
  useGitSummary: () => ({ summary: null, loading: false, refresh: vi.fn() }),
}));
vi.mock('../../src/hooks/useOpenPr', () => ({
  useOpenPr: () => ({ pr: null, loading: false, refresh: vi.fn() }),
}));
vi.mock('@tauri-apps/plugin-opener', () => ({ openUrl: vi.fn().mockResolvedValue(undefined) }));
const { openInFileManagerMock } = vi.hoisted(() => ({
  openInFileManagerMock: vi.fn().mockResolvedValue(undefined),
}));
vi.mock('../../src/lib/tauri', async (importOriginal) => {
  const actual = await importOriginal<typeof import('../../src/lib/tauri')>();
  return { ...actual, openInFileManager: openInFileManagerMock };
});
const { addToastMock } = vi.hoisted(() => ({ addToastMock: vi.fn() }));
vi.mock('../../src/stores/toastStore', () => ({
  addToast: addToastMock,
  dismissToast: vi.fn(),
}));

import { GridNodeHeader } from '../../src/components/AgentNodeView/GridNodeHeader';

const NODE: AgentNode = {
  id: 1,
  mesh_id: 1,
  name: 'agent-1',
  path: '/repo',
  branch: 'main',
  env: 'wsl',
  provider: 'claude',
  status: 'idle',
  use_worktree: false,
  position: 0,
  created_at: new Date(0).toISOString(),
  scratchpad: '',
  sandbox: false,
  is_pinned: false,
};
const MESH: Mesh = {
  id: 1,
  name: 'demo',
  path: '/repo',
  layout: 'single',
  position: 0,
  created_at: new Date(0).toISOString(),
};

function backendProvider(id: string, label: string, extra: Partial<ProviderInfo> = {}): ProviderInfo {
  const [harness_id, provider_id] = id.includes(':') ? (id.split(':') as [string, string]) : [id, null as unknown as string];
  return {
    id,
    label,
    color: '#fff',
    icon: 'x',
    resumable: true,
    harness_id,
    provider_id: id.includes(':') ? provider_id : null,
    is_proxied: id.includes(':'),
    group_key: harness_id,
    capabilities: { supports_model_override: false, supports_effort_override: false, effort_control: 'none' },
    ...extra,
  } as ProviderInfo;
}
const PROVIDERS = [
  backendProvider('claude', 'Claude Code'),
  backendProvider('codex', 'Codex'),
];

function mockProviders(providers: ProviderInfo[] = PROVIDERS) {
  vi.mocked(invoke).mockImplementation((cmd: string) => {
    if (cmd === 'list_providers') return Promise.resolve(providers);
    return Promise.resolve({});
  });
}

function setupState(node: AgentNode = NODE) {
  seedAgentNodes([node], node.id);
  useMeshStore.setState({ meshesById: new Map([[MESH.id, MESH]]), selectedMeshId: MESH.id });
  useUIStore.setState({ viewMode: 'mesh', lastNonSingleMode: 'mesh' });
}

beforeEach(() => {
  roCallbacks.clear();
  vi.stubGlobal('ResizeObserver', FakeResizeObserver);
  __resetProviderCachesForTests();
  vi.mocked(invoke).mockReset();
  addToastMock.mockClear();
  useAgentNodeStore.setState({ nodesById: {}, nodeIds: [], activeNodeId: null });
  vi.spyOn(useAgentNodeStore.getState(), 'regenerateAgentNode').mockImplementation(
    async (nodeId: number, newProviderId: string) => {
      const existing = useAgentNodeStore.getState().nodesById[nodeId];
      return { ...(existing ?? NODE), provider: newProviderId } as AgentNode;
    },
  );
});
afterEach(() => {
  vi.unstubAllGlobals();
});

function makeSpawnOption(id: string, label = id): SpawnOption {
  const harness_id = id.includes(':') ? id.split(':')[0] : id;
  return {
    id,
    label,
    icon: null,
    harness_id,
    provider_id: harness_id,
    is_proxied: false,
    group_key: harness_id,
    color: colorClassForProvider(id),
  };
}

describe('lib/regenerate helpers (issue #1502)', () => {
  it('partitions current from alternates preserving order', () => {
    const list = [makeSpawnOption('claude'), makeSpawnOption('codex'), makeSpawnOption('kimi')];
    const { current, others } = splitRegenerateTargets(list, 'codex');
    expect(current?.id).toBe('codex');
    expect(others.map((o) => o.id)).toEqual(['claude', 'kimi']);
  });
  it('returns undefined current when the provider left the list', () => {
    const { current, others } = splitRegenerateTargets([makeSpawnOption('codex')], 'claude');
    expect(current).toBeUndefined();
    expect(others.map((o) => o.id)).toEqual(['codex']);
  });
  it('hasRegenerateTargets is true for current-only, false for empty', () => {
    expect(hasRegenerateTargets([makeSpawnOption('claude')])).toBe(true);
    expect(hasRegenerateTargets([])).toBe(false);
    expect(hasRegenerateTargets(undefined)).toBe(false);
  });
});

describe('RegenerateProviderMenu (issue #1502)', () => {
  it('pins Current (<label>) on top with data-is-current', () => {
    render(
      <div role="menu">
        <RegenerateProviderMenu
          providers={[makeSpawnOption('claude', 'Claude Code'), makeSpawnOption('codex', 'Codex')]}
          currentProviderId="claude"
          onPick={() => {}}
        />
      </div>,
    );
    const current = screen.getByTestId('regenerate-submenu-current');
    expect(current.textContent).toMatch(/Current \(Claude Code\)/);
    expect(current.getAttribute('data-is-current')).toBe('true');
    expect(current.getAttribute('data-spawn-id')).toBe('claude');
  });
});

describe('GridNodeHeader Regenerate toolbar (issue #1502)', () => {
  it('renders an inline Regenerate button at wide tier whose picker includes the current provider', async () => {
    mockProviders();
    setupState();
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    const root = screen.getByTestId('grid-node-header');
    fireResize(root, 600);
    const btn = await screen.findByTestId('grid-regenerate-button');
    expect(btn.hasAttribute('disabled')).toBe(false);
    await userEvent.click(btn);
    const submenu = await screen.findByTestId('grid-regenerate-submenu');
    expect(submenu).toBeTruthy();
    const current = await screen.findByTestId('grid-regenerate-submenu-current');
    expect(current.textContent).toMatch(/Current \(Claude Code\)/);
  });

  it('picking the current provider in place fires regenerateAgentNode(nodeId, currentId) when idle', async () => {
    mockProviders();
    setupState();
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    fireResize(screen.getByTestId('grid-node-header'), 600);
    await userEvent.click(await screen.findByTestId('grid-regenerate-button'));
    await userEvent.click(await screen.findByTestId('grid-regenerate-submenu-current'));
    await waitFor(() => {
      expect(useAgentNodeStore.getState().regenerateAgentNode).toHaveBeenCalledWith(NODE.id, 'claude');
    });
  });

  it('picking a provider while running opens a confirm dialog; Confirm fires the IPC', async () => {
    mockProviders();
    setupState({ ...NODE, status: 'running' });
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    fireResize(screen.getByTestId('grid-node-header'), 600);
    await userEvent.click(await screen.findByTestId('grid-regenerate-button'));
    await userEvent.click(await screen.findByTestId('grid-regenerate-submenu-current'));
    // Dialog appears instead of immediate IPC.
    expect(await screen.findByText(/Regenerate this node\?/)).toBeTruthy();
    expect(useAgentNodeStore.getState().regenerateAgentNode).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole('button', { name: 'Regenerate' }));
    await waitFor(() => {
      expect(useAgentNodeStore.getState().regenerateAgentNode).toHaveBeenCalledWith(NODE.id, 'claude');
    });
  });

  it('collapses into the kebab menu at slim tier with a Regenerate submenu including current', async () => {
    mockProviders();
    setupState();
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    fireResize(screen.getByTestId('grid-node-header'), 300);
    // Inline button gone, kebab present.
    expect(screen.queryByTestId('grid-regenerate-button')).toBeNull();
    fireEvent.click(screen.getByLabelText('Agent node actions'));
    const trigger = await screen.findByTestId('grid-regenerate-trigger');
    expect(trigger).toBeTruthy();
    await userEvent.click(trigger);
    const submenu = await screen.findByTestId('grid-regenerate-submenu');
    expect(submenu).toBeTruthy();
    expect(await screen.findByTestId('grid-regenerate-submenu-current')).toBeTruthy();
  });

  it('picking current from the kebab submenu fires regenerateAgentNode in place when idle', async () => {
    mockProviders();
    setupState();
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    fireResize(screen.getByTestId('grid-node-header'), 300);
    fireEvent.click(screen.getByLabelText('Agent node actions'));
    await userEvent.click(await screen.findByTestId('grid-regenerate-trigger'));
    await userEvent.click(await screen.findByTestId('grid-regenerate-submenu-current'));
    await waitFor(() => {
      expect(useAgentNodeStore.getState().regenerateAgentNode).toHaveBeenCalledWith(NODE.id, 'claude');
    });
  });

  it('disables the inline button while spawning', async () => {
    mockProviders();
    setupState({ ...NODE, status: 'spawning' });
    render(<GridNodeHeader nodeId={NODE.id} onBuildRun={() => {}} />);
    fireResize(screen.getByTestId('grid-node-header'), 600);
    const btn = await screen.findByTestId('grid-regenerate-button');
    expect(btn.hasAttribute('disabled')).toBe(true);
  });
});
